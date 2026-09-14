use std::collections::HashSet;
use std::io::stdout;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Gauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::{DefaultTerminal, Frame};

use crate::adapters;
use crate::execute::{self, Manifest};
use crate::model::{AgeFilter, CleanMode, CleanPlan, Inventory, Item, Risk};
use crate::optimize::{self, Advice};
use crate::scan;
use crate::ui::anim::{Animated, FRAME};
use crate::ui::hold::{HoldGate, HoldState};
use crate::ui::{modal, theme};
use crate::util;

const LOGO: [&str; 3] = [
    "▄▀█ █▀▀ █▀▀ █▄░█ ▀█▀ █▀ █░█░█ █▀▀ █▀▀ █▀█",
    "█▀█ █▄█ ██▄ █░▀█ ░█░ ▄█ ▀▄▀▄▀ ██▄ ██▄ █▀▀",
    "      understand · control · reclaim     ",
];

#[derive(Clone)]
/// Top-level screens. Modal states (confirm, refused, help, restore,
/// optimize) live in `Overlay`, not here — keep this enum to exactly the
/// screens `draw()` can render.
enum Screen {
    Boot,
    Scan,
    Dash,
    Cleaning,
}

enum Overlay {
    None,
    Confirm,
    Refused,
    Help,
    Restore,
    Optimize,
}

struct App {
    screen: Screen,
    overlay: Overlay,
    started: Instant,
    inventory: Inventory,
    scan_done: bool,
    selected_tool: usize,
    selected_item: usize,
    selected: HashSet<String>,
    mode: CleanMode,
    age: AgeFilter,
    hold: HoldGate,
    anim_total: Animated,
    anim_reclaim: Animated,
    anim_bars: [Animated; 5],
    shake: f64,
    refused: Option<Item>,
    restore: Vec<Manifest>,
    restore_idx: usize,
    advice: Vec<Advice>,
    advice_idx: usize,
    choice_idx: usize,
    picking_choice: bool,
    status: String,
    should_quit: bool,
    clean_progress: f64,
    clean_label: String,
}

impl App {
    fn new() -> Self {
        Self {
            screen: Screen::Boot,
            overlay: Overlay::None,
            started: Instant::now(),
            inventory: Inventory { tools: vec![] },
            scan_done: false,
            selected_tool: 0,
            selected_item: 0,
            selected: HashSet::new(),
            mode: CleanMode::Safe,
            age: AgeFilter::All,
            hold: HoldGate::new(Duration::from_secs(2)),
            anim_total: Animated::new(0.0),
            anim_reclaim: Animated::new(0.0),
            anim_bars: std::array::from_fn(|_| Animated::new(0.0)),
            shake: 0.0,
            refused: None,
            restore: vec![],
            restore_idx: 0,
            advice: vec![],
            advice_idx: 0,
            choice_idx: 0,
            picking_choice: false,
            status: String::new(),
            should_quit: false,
            clean_progress: 0.0,
            clean_label: String::new(),
        }
    }

    fn visible_tools(&self) -> Vec<usize> {
        self.inventory
            .tools
            .iter()
            .enumerate()
            .filter(|(_, t)| t.detected || t.total_bytes() > 0)
            .map(|(i, _)| i)
            .collect()
    }

    fn current_tool_idx(&self) -> Option<usize> {
        let vis = self.visible_tools();
        vis.get(self.selected_tool).copied()
    }

    fn current_items(&self) -> Vec<&Item> {
        let Some(idx) = self.current_tool_idx() else {
            return vec![];
        };
        self.inventory.tools[idx]
            .items
            .iter()
            .filter(|i| i.passes_age(self.age))
            .collect()
    }

    fn selected_items(&self) -> Vec<Item> {
        self.inventory
            .tools
            .iter()
            .flat_map(|t| t.items.iter())
            .filter(|i| self.selected.contains(&i.rule_id) && i.passes_age(self.age))
            .cloned()
            .collect()
    }

    fn reclaimable(&self) -> u64 {
        self.selected_items().iter().map(|i| i.bytes).sum()
    }

    fn retarget_anims(&mut self) {
        self.anim_total.set(self.inventory.total_bytes() as f64);
        self.anim_reclaim.set(self.reclaimable() as f64);
        let total = self.inventory.total_bytes().max(1) as f64;
        let risks = [
            Risk::Safe,
            Risk::Review,
            Risk::Userdata,
            Risk::Critical,
            Risk::Unknown,
        ];
        for (i, r) in risks.iter().enumerate() {
            self.anim_bars[i].set(self.inventory.bytes_by_risk(*r) as f64 / total);
        }
    }

    /// True when every animation has settled, so the dashboard can skip
    /// redraws and idle instead of burning CPU at 60fps.
    fn anims_settled(&self) -> bool {
        const EPS: f64 = 0.5001; // Animated::tick snaps within 0.5
        self.anim_total.value() == self.anim_total.target()
            && self.anim_reclaim.value() == self.anim_reclaim.target()
            && self
                .anim_bars
                .iter()
                .all(|b| (b.value() - b.target()).abs() < EPS)
            && self.shake == 0.0
    }

    fn current_tool_id(&self) -> &str {
        self.current_tool_idx()
            .map(|i| self.inventory.tools[i].id.as_str())
            .unwrap_or("—")
    }

    /// Re-aim the reclaimable counter after the selection changed, so the
    /// footer total eases toward the new value instead of snapping.
    fn refresh_reclaim(&mut self) {
        let r = self.reclaimable();
        self.anim_reclaim.set(r as f64);
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if matches!(self.overlay, Overlay::Confirm) {
            self.handle_confirm_key(key);
            return;
        }
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        match self.overlay {
            Overlay::Refused => {
                self.overlay = Overlay::None;
                self.refused = None;
                return;
            }
            Overlay::Help => {
                self.overlay = Overlay::None;
                return;
            }
            Overlay::Restore => {
                self.handle_restore_key(key);
                return;
            }
            Overlay::Optimize => {
                self.handle_optimize_key(key);
                return;
            }
            Overlay::None | Overlay::Confirm => {}
        }

        if matches!(self.screen, Screen::Boot | Screen::Scan | Screen::Cleaning) {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                self.should_quit = true;
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Up => self.move_item(-1),
            KeyCode::Down => self.move_item(1),
            KeyCode::Left => self.move_tool(-1),
            KeyCode::Right => self.move_tool(1),
            KeyCode::Tab => {
                self.mode = self.mode.next();
                self.selected.retain(|id| {
                    self.inventory
                        .tools
                        .iter()
                        .flat_map(|t| t.items.iter())
                        .any(|i| i.rule_id == *id && self.mode.allows(i.risk))
                });
                self.refresh_reclaim();
            }
            KeyCode::Char('o') => {
                self.age = self.age.cycle();
                self.refresh_reclaim();
            }
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('a') => self.select_allowed(),
            KeyCode::Char('n') => {
                self.selected.clear();
                self.refresh_reclaim();
            }
            KeyCode::Char('d') => self.begin_clean(),
            KeyCode::Char('r') => {
                self.restore = execute::list_quarantines().unwrap_or_default();
                self.restore_idx = 0;
                self.overlay = Overlay::Restore;
            }
            KeyCode::Char('p') => {
                self.advice = optimize::collect();
                self.advice_idx = 0;
                self.choice_idx = 0;
                self.picking_choice = false;
                self.overlay = Overlay::Optimize;
            }
            _ => {}
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = Overlay::None;
                self.hold.reset();
            }
            KeyCode::Char(' ') => {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    self.hold.on_space(Instant::now());
                }
            }
            _ => {
                if key.kind == KeyEventKind::Press {
                    self.overlay = Overlay::None;
                    self.hold.reset();
                }
            }
        }
    }

    fn handle_restore_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Up => self.restore_idx = self.restore_idx.saturating_sub(1),
            KeyCode::Down => {
                if !self.restore.is_empty() {
                    self.restore_idx = (self.restore_idx + 1).min(self.restore.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(m) = self.restore.get(self.restore_idx) {
                    match execute::restore(&m.id) {
                        Ok(n) => self.status = format!("Restored {n} path(s)."),
                        Err(e) => self.status = format!("Restore failed: {e}"),
                    }
                    self.overlay = Overlay::None;
                    self.rescan();
                }
            }
            _ => {}
        }
    }

    fn handle_optimize_key(&mut self, key: KeyEvent) {
        if self.picking_choice {
            let choices = self
                .advice
                .get(self.advice_idx)
                .map(|a| a.choices.len())
                .unwrap_or(0);
            match key.code {
                KeyCode::Esc => self.picking_choice = false,
                KeyCode::Up => self.choice_idx = self.choice_idx.saturating_sub(1),
                KeyCode::Down => {
                    if choices > 0 {
                        self.choice_idx = (self.choice_idx + 1).min(choices - 1);
                    }
                }
                KeyCode::Enter => {
                    if let Some(a) = self.advice.get(self.advice_idx).cloned() {
                        if let Some(c) = a.choices.get(self.choice_idx) {
                            match optimize::apply(&a, &c.value) {
                                Ok(()) => self.status = format!("Set {} to {}.", a.key, c.label),
                                Err(e) => self.status = format!("Failed: {e}"),
                            }
                        }
                    }
                    self.picking_choice = false;
                    self.advice = optimize::collect();
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Up => self.advice_idx = self.advice_idx.saturating_sub(1),
            KeyCode::Down => {
                if !self.advice.is_empty() {
                    self.advice_idx = (self.advice_idx + 1).min(self.advice.len() - 1);
                }
            }
            KeyCode::Enter => {
                self.choice_idx = 0;
                self.picking_choice = true;
            }
            _ => {}
        }
    }

    fn move_item(&mut self, delta: i32) {
        let n = self.current_items().len() as i32;
        if n == 0 {
            return;
        }
        let next = (self.selected_item as i32 + delta).rem_euclid(n) as usize;
        self.selected_item = next;
    }

    fn move_tool(&mut self, delta: i32) {
        let n = self.visible_tools().len() as i32;
        if n == 0 {
            return;
        }
        self.selected_tool = (self.selected_tool as i32 + delta).rem_euclid(n) as usize;
        self.selected_item = 0;
    }

    fn toggle(&mut self) {
        let item = self.current_items().get(self.selected_item).cloned();
        let Some(item) = item.cloned() else { return };
        if item.risk.locked() {
            self.refused = Some(item);
            self.overlay = Overlay::Refused;
            self.shake = 1.0;
            return;
        }
        if !self.mode.allows(item.risk) {
            self.status = format!(
                "Raise mode to {} to select {}.",
                match item.risk {
                    Risk::Review => "SMART",
                    Risk::Userdata => "DEEP",
                    _ => "SAFE",
                },
                item.label
            );
            return;
        }
        if !self.selected.insert(item.rule_id.clone()) {
            self.selected.remove(&item.rule_id);
        }
        self.refresh_reclaim();
    }

    fn select_allowed(&mut self) {
        for tool in &self.inventory.tools {
            for item in &tool.items {
                if self.mode.allows(item.risk) && item.passes_age(self.age) && item.bytes > 0 {
                    self.selected.insert(item.rule_id.clone());
                }
            }
        }
        self.refresh_reclaim();
    }

    fn begin_clean(&mut self) {
        let items = self.selected_items();
        if items.is_empty() {
            self.status = "Nothing selected.".into();
            return;
        }
        if let Err(forbidden) = CleanPlan::try_new(items.clone(), self.mode, false) {
            if let Some(item) = forbidden.into_iter().next() {
                self.refused = Some(item);
                self.overlay = Overlay::Refused;
            }
            return;
        }
        if items
            .iter()
            .any(|i| matches!(i.risk, Risk::Review | Risk::Userdata))
        {
            let secs = if items.iter().any(|i| i.risk == Risk::Userdata) {
                2.0
            } else {
                1.0
            };
            self.hold = HoldGate::new(Duration::from_secs_f64(secs));
            self.overlay = Overlay::Confirm;
        } else {
            self.run_clean(items);
        }
    }

    fn run_clean(&mut self, items: Vec<Item>) {
        self.overlay = Overlay::None;
        self.screen = Screen::Cleaning;
        self.clean_progress = 0.0;
        self.clean_label = items.first().map(|i| i.label.clone()).unwrap_or_default();
        match CleanPlan::try_new(items, self.mode, false) {
            Ok(plan) => match execute::execute(&plan) {
                Ok(report) => {
                    self.status = format!("Reclaimed {}.", util::bytes(report.bytes));
                    self.selected.clear();
                    self.clean_progress = 1.0;
                    self.rescan();
                    self.screen = Screen::Dash;
                }
                Err(e) => {
                    self.status = format!("Clean failed: {e}");
                    self.screen = Screen::Dash;
                }
            },
            Err(_) => {
                self.screen = Screen::Dash;
            }
        }
    }

    fn rescan(&mut self) {
        if let Ok(inv) = scan::inventory(None) {
            self.inventory = inv;
            self.retarget_anims();
        }
    }

    fn tick(&mut self, dt: f64, now: Instant) {
        if matches!(self.screen, Screen::Boot)
            && self.started.elapsed() > Duration::from_millis(700)
        {
            self.screen = Screen::Scan;
        }
        if matches!(self.overlay, Overlay::Confirm)
            && self.hold.tick(now, Duration::from_secs_f64(dt)) == HoldState::Confirmed
        {
            let items = self.selected_items();
            self.run_clean(items);
        }
        self.anim_total.tick(dt);
        self.anim_reclaim.tick(dt);
        for b in &mut self.anim_bars {
            b.tick(dt);
        }
        self.shake *= 0.82;
        if self.shake < 0.02 {
            self.shake = 0.0;
        }
        if self.scan_done && matches!(self.screen, Screen::Scan) {
            self.screen = Screen::Dash;
            self.retarget_anims();
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    // Belt and braces: no block cursor sitting in the middle of the dashboard.
    let _ = terminal.hide_cursor();
    let result = run_app(&mut terminal);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
    let mut app = App::new();
    // Best-effort keyboard-enhancement for key-up/release detection.
    // Falls back to the 150ms key-repeat silence heuristic in hold.rs.
    let _ = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    );
    let (tx, rx) = mpsc::channel::<Inventory>();
    thread::spawn(move || {
        if let Ok(inv) = scan::inventory(None) {
            let _ = tx.send(inv);
        }
    });

    let mut last = Instant::now();
    loop {
        if let Ok(inv) = rx.try_recv() {
            app.inventory = inv;
            app.scan_done = true;
            app.retarget_anims();
        }
        // Idle throttle: once animations settle on the dashboard with no
        // overlay open, stop redrawing at 60fps and wait for input instead.
        let idle = matches!(app.screen, Screen::Dash)
            && matches!(app.overlay, Overlay::None)
            && app.anims_settled();
        if !idle {
            terminal.draw(|f| draw(f, &app))?;
        }
        let timeout = if idle {
            Duration::from_millis(150)
        } else {
            FRAME.saturating_sub(last.elapsed())
        };
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
            terminal.draw(|f| draw(f, &app))?;
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64().min(0.05);
        last = now;
        app.tick(dt, now);
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::Boot => draw_boot(frame, app),
        Screen::Scan => draw_scan(frame, app),
        Screen::Cleaning => draw_cleaning(frame, app),
        _ => draw_dash(frame, app),
    }
    match app.overlay {
        Overlay::Confirm => {
            modal::confirm_modal(
                frame,
                &app.selected_items(),
                app.hold.progress(),
                app.selected_items()
                    .iter()
                    .any(|i| i.risk == Risk::Userdata),
            );
        }
        Overlay::Refused => {
            if let Some(item) = &app.refused {
                modal::refused_modal(frame, item);
            }
        }
        Overlay::Help => modal::help_overlay(frame),
        Overlay::Restore => {
            let opts: Vec<String> = if app.restore.is_empty() {
                vec!["No quarantined snapshots".into()]
            } else {
                app.restore
                    .iter()
                    .map(|m| format!("{}  ·  {} item(s)", m.id, m.items.len()))
                    .collect()
            };
            modal::pick_list(frame, "restore", &opts, app.restore_idx);
        }
        Overlay::Optimize => {
            if app.picking_choice {
                if let Some(a) = app.advice.get(app.advice_idx) {
                    let opts: Vec<String> = a
                        .choices
                        .iter()
                        .map(|c| {
                            if c.value == a.current {
                                format!("{}  (current)", c.label)
                            } else {
                                c.label.clone()
                            }
                        })
                        .collect();
                    modal::pick_list(frame, &format!("set {}", a.key), &opts, app.choice_idx);
                }
            } else {
                let opts: Vec<String> = app
                    .advice
                    .iter()
                    .map(|a| format!("{}  {} = {}", a.tool, a.key, a.current))
                    .collect();
                modal::pick_list(frame, "optimize", &opts, app.advice_idx);
            }
        }
        Overlay::None => {}
    }
}

fn draw_boot(frame: &mut Frame, app: &App) {
    let elapsed = app.started.elapsed().as_secs_f64();
    let t = (elapsed / 0.4).clamp(0.0, 1.0);
    let area = frame.area();
    let y = area.height / 2 - 2;
    for (i, line) in LOGO.iter().enumerate() {
        let shown: String = line
            .chars()
            .take((line.chars().count() as f64 * t) as usize)
            .collect();
        let spans: Vec<Span> = shown
            .chars()
            .enumerate()
            .map(|(ci, ch)| {
                let g = ci as f64 / line.len().max(1) as f64;
                Span::styled(ch.to_string(), Style::default().fg(theme::gradient(g)))
            })
            .collect();
        let rect = Rect::new(area.x, area.y + y + i as u16, area.width, 1);
        frame.render_widget(Paragraph::new(Line::from(spans)).centered(), rect);
    }
}

fn draw_scan(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" scanning ")
        .title_style(theme::title())
        .border_style(Style::default().fg(theme::CYAN));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let idx = (app.started.elapsed().as_millis() / 80) as usize % spinner.len();
    let tools: Vec<&'static str> = adapters::all().iter().map(|a| a.id()).collect();
    let step = (inner.height.saturating_sub(2) / tools.len().max(1) as u16).clamp(1, 3);
    for (i, name) in tools.iter().enumerate() {
        let found = app.inventory.tools.iter().find(|t| t.id == *name);
        let bytes = found.map(|t| t.total_bytes()).unwrap_or(0);
        let ratio = if app.scan_done {
            1.0
        } else {
            ((app.started.elapsed().as_secs_f64() / 1.2) - i as f64 * 0.15).clamp(0.0, 0.92)
        };
        let gauge = Gauge::default()
            .block(Block::default().title(format!(" {} {} ", spinner[idx], name)))
            .gauge_style(Style::default().fg(theme::CYAN))
            .ratio(ratio)
            .label(util::bytes(bytes));
        let row = Rect::new(
            inner.x + 2,
            inner.y + 2 + (i as u16) * step,
            inner.width.saturating_sub(4),
            step.saturating_sub(1).max(1),
        );
        frame.render_widget(gauge, row);
    }
}

fn draw_cleaning(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" cleaning ")
        .title_style(theme::title())
        .border_style(Style::default().fg(theme::GREEN));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(theme::GREEN))
        .ratio(app.clean_progress.clamp(0.0, 1.0))
        .label(app.clean_label.clone());
    let row = Rect::new(
        inner.x + 4,
        inner.y + inner.height / 2,
        inner.width.saturating_sub(8),
        3,
    );
    frame.render_widget(gauge, row);
}

fn draw_dash(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(7),
            Constraint::Min(6),
            Constraint::Length(4),
            Constraint::Length(2),
        ])
        .split(area);

    draw_header(frame, app, chunks[0]);
    draw_tools(frame, app, chunks[1]);
    draw_items(frame, app, chunks[2]);
    draw_detail(frame, app, chunks[3]);
    draw_footer(frame, app, chunks[4]);
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let mode = |m: CleanMode, label: &str| {
        if app.mode == m {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(theme::BG)
                    .bg(theme::CYAN)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), theme::dim())
        }
    };
    let age = |a: AgeFilter, label: &str| {
        if app.age == a {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(theme::CYAN)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), theme::dim())
        }
    };
    let line = Line::from(vec![
        Span::styled(" AGENTSWEEP ", theme::title()),
        Span::raw("  "),
        mode(CleanMode::Safe, "SAFE"),
        Span::styled("·", theme::dim()),
        mode(CleanMode::Smart, "SMART"),
        Span::styled("·", theme::dim()),
        mode(CleanMode::Deep, "DEEP"),
        Span::raw("    "),
        age(AgeFilter::All, "All"),
        age(AgeFilter::Days(7), ">7d"),
        age(AgeFilter::Days(30), ">30d"),
        age(AgeFilter::Days(90), ">90d"),
        Span::raw("    "),
        Span::styled(
            format!("total {}", util::bytes(app.anim_total.value() as u64)),
            theme::fg(),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::CYAN)),
        ),
        area,
    );
}

fn draw_tools(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::GREY));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let vis = app.visible_tools();
    let legend_row = if inner.height >= 5 { 1 } else { 0 };
    let visible_rows = inner.height.saturating_sub(legend_row) as usize;
    let start = scroll_start(app.selected_tool, vis.len(), visible_rows);
    for (row, &idx) in vis.iter().enumerate().skip(start).take(visible_rows) {
        let tool = &app.inventory.tools[idx];
        let selected = row == app.selected_tool;
        let y = inner.y + (row - start) as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let marker = if selected { "▶" } else { " " };
        let running = if tool.running { " ●" } else { "" };
        let name = format!(
            "{marker} {:<10} {:>8}{running}",
            tool.id,
            util::bytes(tool.total_bytes())
        );
        let style = if selected {
            Style::default()
                .fg(theme::CYAN)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::fg()
        };
        frame.render_widget(
            Paragraph::new(Span::styled(name, style)),
            Rect::new(inner.x + 1, y, 28.min(inner.width), 1),
        );
        if inner.width > 32 {
            let bar_area = Rect::new(inner.x + 30, y, inner.width.saturating_sub(32), 1);
            draw_stack_bar(frame, app, bar_area, tool);
        }
    }
    if vis.len() > visible_rows {
        let mut sb_state = ScrollbarState::new(vis.len().saturating_sub(visible_rows))
            .position(start)
            .viewport_content_length(visible_rows);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(theme::GREY));
        frame.render_stateful_widget(
            scrollbar,
            Rect::new(area.x, inner.y, area.width, visible_rows as u16),
            &mut sb_state,
        );
    }
    // Legend row pinned to the bottom of the tools panel: bar colors and
    // live per-risk totals across all tools.
    if inner.height >= 5 {
        let ly = inner.y + inner.height - 1;
        let legend = |risk: Risk, name: &'static str| {
            vec![
                Span::styled(" ■ ", Style::default().fg(theme::risk_color(risk))),
                Span::styled(name, theme::dim()),
                Span::styled(
                    format!(" {}", util::bytes(app.inventory.bytes_by_risk(risk))),
                    theme::fg(),
                ),
                Span::raw("   "),
            ]
        };
        let mut spans = vec![Span::raw(" ")];
        spans.extend(legend(Risk::Safe, "safe"));
        spans.extend(legend(Risk::Review, "review"));
        spans.extend(legend(Risk::Userdata, "user"));
        spans.extend(legend(Risk::Critical, "locked"));
        spans.extend(legend(Risk::Unknown, "unknown"));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(inner.x + 1, ly, inner.width.saturating_sub(2), 1),
        );
    }
}

fn draw_stack_bar(frame: &mut Frame, _app: &App, area: Rect, tool: &crate::model::ToolInventory) {
    let total = tool.total_bytes().max(1) as f64;
    let parts = [
        (tool.bytes_by_risk(Risk::Safe), theme::GREEN),
        (tool.bytes_by_risk(Risk::Review), theme::YELLOW),
        (tool.bytes_by_risk(Risk::Userdata), theme::ORANGE),
        (tool.bytes_by_risk(Risk::Critical), theme::RED),
        (tool.bytes_by_risk(Risk::Unknown), theme::GREY),
    ];
    let width = area.width as usize;
    let mut spans = Vec::new();
    let mut used = 0usize;
    for (i, (bytes, color)) in parts.iter().enumerate() {
        let mut w = ((*bytes as f64 / total) * width as f64).round() as usize;
        if i == parts.len() - 1 {
            w = width.saturating_sub(used);
        }
        used += w;
        if w > 0 {
            spans.push(Span::styled("▀".repeat(w), Style::default().fg(*color)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Detail pane: the full story on the highlighted item, so the table can
/// stay compact without losing information to truncation.
fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" detail ")
        .title_style(theme::title())
        .border_style(Style::default().fg(theme::GREY));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let w = inner.width as usize;
    let (l1, l2): (Line, Line) = match app.current_items().get(app.selected_item) {
        Some(item) => {
            let color = if item.risk.locked() {
                theme::DIM
            } else {
                theme::risk_color(item.risk)
            };
            let head = Line::from(vec![
                Span::styled(
                    format!("{}  ", item.label),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{}  ", util::bytes(item.bytes)), theme::fg()),
                Span::styled(item.risk.label(), Style::default().fg(color)),
            ]);
            let path = item
                .paths
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let body = if path.is_empty() {
                trunc(&item.consequence, w)
            } else {
                trunc(&format!("{}  —  {}", item.consequence, path), w)
            };
            (head, Line::from(Span::styled(body, theme::dim())))
        }
        None => (
            Line::from(Span::styled("Nothing here.", theme::fg())),
            Line::from(Span::styled(
                "Pick a tool with ← →, raise the mode with tab, select with space.",
                theme::dim(),
            )),
        ),
    };
    frame.render_widget(
        Paragraph::new(l1),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if inner.height > 1 {
        frame.render_widget(
            Paragraph::new(l2),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

/// Top visible row so `selected` stays inside a `visible`-tall window over
/// `total` rows, roughly centered rather than pinned to an edge.
fn scroll_start(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    selected.saturating_sub(visible / 2).min(total - visible)
}

fn draw_items(frame: &mut Frame, app: &App, area: Rect) {
    let items = app.current_items();
    let visible_rows = area.height.saturating_sub(2) as usize;
    let start = scroll_start(app.selected_item, items.len(), visible_rows);
    let title = if items.len() > visible_rows {
        format!(
            " {} items ({}-{} of {}) ",
            app.current_tool_id(),
            start + 1,
            (start + visible_rows).min(items.len()),
            items.len()
        )
    } else {
        format!(" {} items ", app.current_tool_id())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(theme::title())
        .border_style(Style::default().fg(theme::GREY));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let shake_off = if app.shake > 0.0 {
        ((app.shake * 3.0).sin() * 2.0) as i16
    } else {
        0
    };
    for (i, item) in items.iter().enumerate().skip(start).take(visible_rows) {
        let y = inner.y + (i - start) as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let checked = app.selected.contains(&item.rule_id);
        let glyph = if item.risk.locked() {
            "🔒"
        } else if checked {
            "✓ "
        } else {
            "○ "
        };
        let hl = i == app.selected_item;
        let mut style = if item.risk.locked() {
            theme::dim()
        } else {
            Style::default().fg(theme::risk_color(item.risk))
        };
        if hl {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let line = format!(
            "{glyph} {:<26} {:>10}  {:<10}  {}",
            trunc(&item.label, 26),
            util::bytes(item.bytes),
            item.risk.label(),
            trunc(&item.consequence, inner.width.saturating_sub(52) as usize)
        );
        let x = if hl && shake_off != 0 {
            inner.x.saturating_add_signed(shake_off.max(0))
        } else {
            inner.x + 1
        };
        frame.render_widget(
            Paragraph::new(Span::styled(line, style)),
            Rect::new(x, y, inner.width.saturating_sub(2), 1),
        );
    }
    if items.len() > visible_rows {
        let mut sb_state = ScrollbarState::new(items.len().saturating_sub(visible_rows))
            .position(start)
            .viewport_content_length(visible_rows);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(theme::GREY));
        frame.render_stateful_widget(scrollbar, area, &mut sb_state);
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let reclaim_bytes = app.anim_reclaim.value();
    let reclaim = util::bytes(reclaim_bytes as u64);
    let reclaim_style = if reclaim_bytes > 0.5 {
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD)
    } else {
        theme::dim()
    };
    let mut spans = vec![
        Span::styled(" reclaimable ", theme::dim()),
        Span::styled(reclaim, reclaim_style),
        Span::styled("  │ ", Style::default().fg(theme::GREY)),
    ];
    if app.status.is_empty() {
        let keys = [
            ("space", "toggle"),
            ("a", "select"),
            ("d", "clean"),
            ("r", "restore"),
            ("p", "optimize"),
            ("?", "help"),
            ("q", "quit"),
        ];
        for (i, (key, label)) in keys.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            spans.extend(theme::shortcut(key, label));
        }
    } else {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(app.status.clone(), theme::fg()));
        spans.push(Span::raw("  "));
        spans.extend(theme::shortcut("?", "help"));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn trunc(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    if s.chars().count() <= n {
        return s.to_string();
    }
    let t: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{t}…")
}

#[cfg(test)]
mod dash_tests {
    use super::*;
    use crate::model::ToolInventory;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn test_app() -> App {
        let mut app = App::new();
        let item = Item {
            rule_id: "claude.plans".into(),
            tool: "claude".into(),
            label: "Old plan files".into(),
            paths: vec![PathBuf::from("/tmp/plans")],
            bytes: 143_360,
            risk: Risk::Safe,
            requires_stopped: false,
            consequence: "Deletes old plan-mode files.".into(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: None,
        };
        app.inventory = Inventory {
            tools: vec![ToolInventory {
                id: "claude".into(),
                version: Some("2.1.270".into()),
                detected: true,
                running: false,
                roots: BTreeMap::new(),
                items: vec![item],
            }],
        };
        app.screen = Screen::Dash;
        app.scan_done = true;
        app.retarget_anims();
        for _ in 0..500 {
            app.anim_total.tick(0.016);
            app.anim_reclaim.tick(0.016);
            for b in &mut app.anim_bars {
                b.tick(0.016);
            }
        }
        app
    }

    fn rendered(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn dash_shows_tool_title_legend_and_detail() {
        let app = test_app();
        let text = rendered(&app, 120, 30);
        assert!(text.contains("claude items"), "items panel names the tool");
        assert!(text.contains("Old plan files"), "item row renders");
        assert!(text.contains("detail"), "detail pane renders");
        assert!(
            text.contains("Deletes old plan-mode files."),
            "detail pane shows the full consequence"
        );
        for word in ["safe", "review", "user", "locked", "unknown"] {
            assert!(text.contains(word), "legend shows {word}");
        }
        assert!(text.contains("reclaimable"), "footer renders");
        for word in [
            "toggle", "select", "clean", "restore", "optimize", "help", "quit",
        ] {
            assert!(text.contains(word), "footer shortcut shows {word}");
        }
    }

    #[test]
    fn animations_settle_so_idle_throttle_kicks_in() {
        let app = test_app();
        assert!(
            app.anims_settled(),
            "settled animations must read as settled for the idle path"
        );
    }

    #[test]
    fn scroll_start_keeps_selection_in_window() {
        assert_eq!(scroll_start(0, 5, 10), 0, "fits entirely, no scroll");
        assert_eq!(scroll_start(0, 100, 10), 0, "top stays at 0");
        assert_eq!(scroll_start(99, 100, 10), 90, "bottom clamps to the end");
        let start = scroll_start(50, 100, 10);
        assert!(
            (start..start + 10).contains(&50),
            "selection {} must stay inside window [{start}, {})",
            50,
            start + 10
        );
    }

    #[test]
    fn deep_selection_scrolls_into_view() {
        let mut app = test_app();
        let mut items = Vec::new();
        for i in 0..60 {
            items.push(Item {
                rule_id: format!("t.item{i}"),
                tool: "claude".into(),
                label: format!("Item number {i}"),
                paths: vec![],
                bytes: 4096,
                risk: Risk::Safe,
                requires_stopped: false,
                consequence: String::new(),
                oldest_mtime: None,
                newest_mtime: None,
                delegate: None,
            });
        }
        app.inventory.tools[0].items = items;
        app.selected_item = 55;

        let text = rendered(&app, 120, 30);
        assert!(
            text.contains("Item number 55"),
            "the selected row must scroll into view instead of rendering off-screen"
        );
        assert!(
            text.contains("of 60"),
            "the items panel should show a scroll position indicator"
        );
    }

    #[test]
    fn selection_moves_reclaimable_target() {
        let mut app = test_app();
        assert_eq!(app.anim_reclaim.target(), 0.0);
        app.toggle();
        assert_eq!(app.anim_reclaim.target(), 143_360.0);
        assert!(
            !app.anims_settled(),
            "fresh selection must keep frames flowing"
        );
    }
}
