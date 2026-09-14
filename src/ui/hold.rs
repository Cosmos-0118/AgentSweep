use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoldState {
    Armed,
    Filling(f64),
    Draining(f64),
    Confirmed,
}

#[derive(Debug, Clone)]
pub struct HoldGate {
    progress: f64,
    last_press: Option<Instant>,
    required: Duration,
    silence: Duration,
    confirmed: bool,
}

impl HoldGate {
    pub fn new(required: Duration) -> Self {
        Self {
            progress: 0.0,
            last_press: None,
            required,
            silence: Duration::from_millis(150),
            confirmed: false,
        }
    }

    pub fn reset(&mut self) {
        self.progress = 0.0;
        self.last_press = None;
        self.confirmed = false;
    }

    pub fn on_space(&mut self, now: Instant) {
        self.last_press = Some(now);
    }

    pub fn tick(&mut self, now: Instant, dt: Duration) -> HoldState {
        if self.confirmed {
            return HoldState::Confirmed;
        }
        let holding = self
            .last_press
            .map(|t| now.saturating_duration_since(t) <= self.silence)
            .unwrap_or(false);
        let req = self.required.as_secs_f64().max(0.001);
        let step = dt.as_secs_f64() / req;
        if holding {
            self.progress = (self.progress + step).min(1.0);
            if self.progress >= 1.0 {
                self.confirmed = true;
                return HoldState::Confirmed;
            }
            HoldState::Filling(self.progress)
        } else if self.progress > 0.0 {
            self.progress = (self.progress - 3.0 * step).max(0.0);
            if self.progress == 0.0 {
                HoldState::Armed
            } else {
                HoldState::Draining(self.progress)
            }
        } else {
            HoldState::Armed
        }
    }

    pub fn progress(&self) -> f64 {
        self.progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirms_after_required_hold() {
        let mut g = HoldGate::new(Duration::from_secs(2));
        let mut t = Instant::now();
        g.on_space(t);
        let mut state = HoldState::Armed;
        for _ in 0..20 {
            t += Duration::from_millis(120);
            g.on_space(t);
            state = g.tick(t, Duration::from_millis(120));
        }
        assert_eq!(state, HoldState::Confirmed);
        assert!((g.progress() - 1.0).abs() < f64::EPSILON || g.progress() >= 1.0);
    }

    #[test]
    fn short_hold_then_release_does_not_confirm() {
        let mut g = HoldGate::new(Duration::from_secs(2));
        let mut t = Instant::now();
        g.on_space(t);
        for _ in 0..8 {
            t += Duration::from_millis(120);
            g.on_space(t);
            let state = g.tick(t, Duration::from_millis(120));
            assert_ne!(state, HoldState::Confirmed);
        }
        // 1s of silence: drain at 3x, so 1s * 3 / 2s = 1.5 of progress lost
        for _ in 0..12 {
            t += Duration::from_millis(120);
            let state = g.tick(t, Duration::from_millis(120));
            if matches!(state, HoldState::Confirmed) {
                panic!("confirmed after release");
            }
        }
        assert!(g.progress() < 0.05);
        assert_ne!(g.tick(t, Duration::from_millis(16)), HoldState::Confirmed);
    }

    #[test]
    fn review_hold_is_one_second() {
        let mut g = HoldGate::new(Duration::from_secs(1));
        let mut t = Instant::now();
        g.on_space(t);
        for _ in 0..10 {
            t += Duration::from_millis(120);
            g.on_space(t);
            if g.tick(t, Duration::from_millis(120)) == HoldState::Confirmed {
                return;
            }
        }
        panic!("1s hold did not confirm");
    }
}
