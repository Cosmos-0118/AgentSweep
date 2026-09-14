use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Animated {
    current: f64,
    target: f64,
}

impl Animated {
    pub fn new(value: f64) -> Self {
        Self {
            current: value,
            target: value,
        }
    }

    pub fn set(&mut self, target: f64) {
        self.target = target;
    }

    pub fn snap(&mut self, value: f64) {
        self.current = value;
        self.target = value;
    }

    pub fn tick(&mut self, dt: f64) {
        // Exponential ease-out toward target.
        self.current += (self.target - self.current) * (1.0 - (-8.0 * dt).exp());
        if (self.current - self.target).abs() < 0.5 {
            self.current = self.target;
        }
    }

    pub fn value(&self) -> f64 {
        self.current
    }

    pub fn target(&self) -> f64 {
        self.target
    }
}

pub const FRAME: Duration = Duration::from_millis(16);
