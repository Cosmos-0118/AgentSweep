use std::collections::HashSet;
use std::io::stdout;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
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
const REFRESH_INTERVAL: Duration = Duration::from_secs(15);
// A full scan walks every watched root on disk; once the picture is stable,
// re-walking on a fixed 15s clock forever is wasted CPU/disk work for as
// long as the app sits open. Back off exponentially up to this cap and reset
// to REFRESH_INTERVAL the moment a scan actually finds something changed.
const MAX_REFRESH_INTERVAL: Duration = Duration::from_secs(120);
const FADE_DURATION: Duration = Duration::from_millis(650);
const MARQUEE_PAUSE: Duration = Duration::from_millis(1_200);
const MARQUEE_STEP: Duration = Duration::from_millis(120);
const MARQUEE_SEPARATOR: &[char] = &[' ', ' ', '─', '─', ' ', '↺', ' ', '─', '─', ' ', ' '];

#[derive(Clone)]
/// Top-level screens. Modal states (confirm, refused, help, restore,
/// optimize) live in `Overlay`, not here — keep this enum to exactly the
/// screens `draw()` can render.
enum Screen {
    Boot,
    Scan,
    Dash,
    Fading,
    Reconciling,
    Cleaning,
}

enum Overlay {
    None,
    Confirm,
    Refused,
    Notice,
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
    notice: Option<Notice>,
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
    clean_started: Option<Instant>,
    clean_running: bool,
    clean_handle: Option<thread::JoinHandle<()>>,
    clean_tx: Option<mpsc::Sender<anyhow::Result<execute::ExecReport>>>,
    refresh_in_flight: bool,
    refresh_pending: bool,
    refresh_handle: Option<thread::JoinHandle<()>>,
    next_refresh: Instant,
    refresh_interval: Duration,
    pending_clean: Option<Vec<Item>>,
    skipped_clean_rules: HashSet<String>,
    fade_started: Option<Instant>,
    marquee_started: Instant,
}

struct Notice {
    title: String,
    message: String,
    color: ratatui::style::Color,
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
            notice: None,
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
            clean_started: None,
            clean_running: false,
            clean_handle: None,
            clean_tx: None,
            refresh_in_flight: false,
            refresh_pending: false,
            refresh_handle: None,
            next_refresh: Instant::now(),
            refresh_interval: REFRESH_INTERVAL,
            pending_clean: None,
            skipped_clean_rules: HashSet::new(),
            fade_started: None,
            marquee_started: Instant::now(),
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
    /// redraws and idle instead of burning CPU at 60fps. A live source keeps
    /// its runner moving, so it deliberately prevents that idle path.
    fn anims_settled(&self) -> bool {
        const EPS: f64 = 0.5001; // Animated::tick snaps within 0.5
        self.anim_total.value() == self.anim_total.target()
            && self.anim_reclaim.value() == self.anim_reclaim.target()
            && self
                .anim_bars
                .iter()
                .all(|b| (b.value() - b.target()).abs() < EPS)
            && self.shake == 0.0
            && !self.inventory.tools.iter().any(|tool| tool.running)
    }

    fn marquee_active(&self, terminal_width: usize) -> bool {
        let items = self.current_items();
        let description_width = item_description_width(&items, terminal_width.saturating_sub(2));
        items
            .get(self.selected_item)
            .is_some_and(|item| item.consequence.chars().count() > description_width)
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

    /// Labels of just-cleaned items that a fresh, post-clean scan still finds
    /// with real bytes on disk - i.e. the move reported success but did not
    /// stick. `pending_clean` is only Some between a clean starting and its
    /// reconciliation scan landing, so this only ever fires for that window.
    fn reappeared_after_clean(&self, inventory: &Inventory) -> Vec<String> {
        let Some(items) = &self.pending_clean else {
            return vec![];
        };
        items
            .iter()
            .filter(|pending| {
                !self.skipped_clean_rules.contains(&pending.rule_id)
                    && inventory
                        .tools
                        .iter()
                        .flat_map(|t| t.items.iter())
                        .any(|fresh| fresh.rule_id == pending.rule_id && fresh.bytes > 0)
            })
            .map(|item| item.label.clone())
            .collect()
    }

    /// Replace the view only after a successful scan, while preserving
    /// navigation and removing selections whose backing item is gone.
    fn apply_inventory(&mut self, inventory: Inventory) {
        let tool_id = self.current_tool_id().to_string();
        let item_id = self
            .current_items()
            .get(self.selected_item)
            .map(|item| item.rule_id.clone());
        self.inventory = inventory;
        self.selected.retain(|id| {
            self.inventory
                .tools
                .iter()
                .flat_map(|tool| tool.items.iter())
                .any(|item| item.rule_id == *id && self.mode.allows(item.risk))
        });
        let visible = self.visible_tools();
        self.selected_tool = visible
            .iter()
            .position(|&index| self.inventory.tools[index].id == tool_id)
            .unwrap_or(0);
        let items = self.current_items();
        self.selected_item = item_id
            .as_ref()
            .and_then(|id| items.iter().position(|item| item.rule_id == *id))
            .unwrap_or(0)
            .min(items.len().saturating_sub(1));
        self.retarget_anims();
    }

    fn request_refresh(&mut self) {
        self.refresh_pending = true;
    }

    fn refresh_due(&self, now: Instant) -> bool {
        matches!(self.screen, Screen::Dash | Screen::Reconciling)
            && matches!(self.overlay, Overlay::None)
            && !self.refresh_in_flight
            && (self.refresh_pending || now >= self.next_refresh)
    }

    /// How long to wait before the next periodic scan. `changed` covers a
    /// scan that found different reclaimable bytes, a scan that failed or
    /// crashed (worth retrying soon rather than waiting out a long backoff),
    /// and a clean's reconciliation scan - even one that found the exact
    /// pre-clean picture, since that's the "delete did not stick" case that
    /// needs closer watching, not less.
    fn next_refresh_interval(&self, changed: bool) -> Duration {
        if changed {
            REFRESH_INTERVAL
        } else {
            (self.refresh_interval * 2).min(MAX_REFRESH_INTERVAL)
        }
    }

    fn fade_progress(&self) -> Option<f64> {
        self.fade_started.map(|started| {
            (started.elapsed().as_secs_f64() / FADE_DURATION.as_secs_f64()).clamp(0.0, 1.0)
        })
    }

    fn begin_fade(&mut self, items: Vec<Item>) {
        self.overlay = Overlay::None;
        self.pending_clean = Some(items);
        self.fade_started = Some(Instant::now());
        self.screen = Screen::Fading;
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
            Overlay::Notice => {
                self.overlay = Overlay::None;
                self.notice = None;
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

        if matches!(
            self.screen,
            Screen::Boot | Screen::Scan | Screen::Fading | Screen::Reconciling | Screen::Cleaning
        ) {
            if matches!(self.screen, Screen::Fading)
                && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
            {
                self.pending_clean = None;
                self.fade_started = None;
                self.screen = Screen::Dash;
                self.status = "Cleanup cancelled.".into();
                return;
            }
            // Once files are moving there is no safe rollback point: quitting
            // here would abandon a partially-applied plan. Quit is re-enabled
            // the moment the worker reports back and the screen leaves Cleaning.
            if matches!(self.screen, Screen::Cleaning)
                && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
            {
                self.status = "Cleanup in progress; please wait for it to finish.".into();
                return;
            }
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
            KeyCode::Enter => self.open_current_item(),
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
            KeyCode::Char('u') => self.request_refresh(),
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
                    self.request_refresh();
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
        self.marquee_started = Instant::now();
    }

    fn move_tool(&mut self, delta: i32) {
        let n = self.visible_tools().len() as i32;
        if n == 0 {
            return;
        }
        self.selected_tool = (self.selected_tool as i32 + delta).rem_euclid(n) as usize;
        self.selected_item = 0;
        self.marquee_started = Instant::now();
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

    /// Open the primary path shown in the detail pane in the file manager. The
    /// dashboard only ever passes a `PathBuf` as an argument, never through a
    /// shell, so unusual filenames stay safe.
    fn open_current_item(&mut self) {
        let Some(item) = self.current_items().get(self.selected_item).cloned() else {
            self.status = "Nothing to open.".into();
            return;
        };
        let Some(path) = item.paths.first() else {
            self.status = format!("No path available for {}.", item.label);
            return;
        };

        // `output` captures launcher diagnostics. Using `status` here lets
        // macOS write errors into the alternate screen/scrollback, corrupting
        // the TUI when a file has no associated application.
        let result = if cfg!(target_os = "macos") {
            let mut command = Command::new("open");
            // A directory opens as a Finder window; a file is revealed and
            // selected in Finder. Neither path depends on a file association.
            if !path.is_dir() {
                command.arg("-R");
            }
            command.arg(path).output()
        } else if cfg!(target_os = "windows") {
            Command::new("explorer").arg(path).output()
        } else {
            Command::new("xdg-open").arg(path).output()
        };

        match result {
            Ok(output) if output.status.success() => {
                self.status = format!("Shown in Finder: {}.", path.display());
            }
            Err(error) => {
                self.show_notice(
                    " COULDN'T OPEN ",
                    format!("AgentSweep could not open {}: {error}", path.display()),
                    theme::RED,
                );
            }
            Ok(_) => self.show_notice(
                " COULDN'T OPEN ",
                format!("AgentSweep could not open {}.", path.display()),
                theme::RED,
            ),
        }
    }

    fn show_notice(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
        color: ratatui::style::Color,
    ) {
        self.notice = Some(Notice {
            title: title.into(),
            message: message.into(),
            color,
        });
        self.overlay = Overlay::Notice;
    }

    fn select_allowed(&mut self) {
        let applicable: Vec<String> = self
            .inventory
            .tools
            .iter()
            .flat_map(|tool| tool.items.iter())
            .filter(|item| {
                self.mode.allows(item.risk) && item.passes_age(self.age) && item.bytes > 0
            })
            .map(|item| item.rule_id.clone())
            .collect();
        let all_selected = !applicable.is_empty()
            && applicable
                .iter()
                .all(|rule_id| self.selected.contains(rule_id));

        if all_selected {
            for rule_id in applicable {
                self.selected.remove(&rule_id);
            }
        } else {
            self.selected.extend(applicable);
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
            self.begin_fade(items);
        }
    }

    /// Hand the actual deletion/quarantine work to a worker thread. The real
    /// work here is filesystem I/O and, for delegated items, external CLI
    /// calls - either can take long enough that running it inline would
    /// freeze the render loop and stop it from polling input, which reads to
    /// a user exactly like a crash. Backgrounding it keeps the loop free to
    /// keep drawing and responding to keys for the whole duration.
    fn run_clean(&mut self, items: Vec<Item>) {
        self.fade_started = None;
        self.overlay = Overlay::None;
        self.screen = Screen::Cleaning;
        self.clean_progress = 0.0;
        self.clean_started = Some(Instant::now());
        self.clean_label = items.first().map(|i| i.label.clone()).unwrap_or_default();
        self.skipped_clean_rules.clear();
        // A scan already in flight (or one the periodic timer fires next) must
        // never land mid-clean and be applied over paths that are actively
        // being moved or deleted; the existing "pending refresh" discard path
        // covers exactly this once a fresh scan is requested here.
        self.request_refresh();
        match CleanPlan::try_new(items, self.mode, false) {
            Ok(plan) => {
                let Some(tx) = self.clean_tx.clone() else {
                    self.pending_clean = None;
                    self.screen = Screen::Dash;
                    return;
                };
                self.clean_running = true;
                self.clean_handle = Some(thread::spawn(move || {
                    let _ = tx.send(execute::execute(&plan));
                }));
            }
            Err(_) => {
                self.pending_clean = None;
                self.screen = Screen::Dash;
            }
        }
    }

    /// Apply a finished (or crashed) clean worker's outcome. Shared by the
    /// normal result path and the worker-panicked path so both leave the app
    /// in the same recoverable state instead of one of them wedging the UI.
    fn finish_clean(&mut self, result: anyhow::Result<execute::ExecReport>) {
        self.clean_running = false;
        self.clean_handle = None;
        match result {
            Ok(report) => {
                self.skipped_clean_rules = report.skipped_rule_ids.iter().cloned().collect();
                self.status = if report.skipped.is_empty() {
                    format!("Reclaimed {}.", util::bytes(report.bytes))
                } else {
                    format!(
                        "Reclaimed {}; skipped {}. Refreshing actual state.",
                        util::bytes(report.bytes),
                        report.skipped.join("; ")
                    )
                };
                // Keep the selection through reconciliation. Rows actually
                // cleaned disappear from the fresh inventory automatically;
                // rows skipped because their tool is running remain selected
                // so the user can retry them after closing that tool.
            }
            Err(e) => {
                self.status = format!("Clean failed: {e}");
            }
        }
        self.clean_progress = 1.0;
        // Regardless of outcome, never trust the pre-clean snapshot again:
        // read the filesystem fresh before showing anything as done.
        self.request_refresh();
        self.screen = Screen::Reconciling;
    }

    /// Apply a scan result once a refresh is known not to be stale (a caller
    /// checks `!refresh_pending` before calling this). Pulled out of the
    /// event loop so the `Fading` guard below is covered by a unit test
    /// instead of only by manually racing a background scan against a hold.
    fn apply_refresh_result(&mut self, result: anyhow::Result<Inventory>) {
        match result {
            Ok(inv) => {
                if self.status.starts_with("Refresh failed;") {
                    self.status.clear();
                }
                // The move/quarantine itself can report success and still
                // not stick (a rule matching more than one on-disk location
                // for the same data is one concrete way that happens).
                // Reconciling's whole job is to check reality rather than
                // trust that report, so state the fact plainly instead of
                // silently leaving the "Reclaimed" status next to a row that
                // quietly came back - without guessing at a cause this
                // can't actually verify.
                let reconciling = matches!(self.screen, Screen::Reconciling);
                if reconciling {
                    let reappeared = self.reappeared_after_clean(&inv);
                    if !reappeared.is_empty() {
                        self.status = format!(
                            "{} still present after cleanup - the delete did not stick.",
                            reappeared.join(", ")
                        );
                    }
                }
                // A reconciliation scan that still matches the pre-clean
                // picture is the "delete did not stick" case - exactly the
                // one that needs closer watching, not a longer backoff.
                let changed = reconciling || !inv.same_reclaimable_shape(&self.inventory);
                self.refresh_interval = self.next_refresh_interval(changed);
                self.apply_inventory(inv);
                // A scan that was already in flight when the hold-confirm
                // opened can land just after it confirms, while `Fading` is
                // holding `pending_clean` for the clean that hasn't started
                // yet. Clearing it here would silently drop that clean and
                // leave `Fading` waiting forever with nothing left to run.
                if !matches!(self.screen, Screen::Fading) {
                    self.pending_clean = None;
                    self.skipped_clean_rules.clear();
                }
                if reconciling {
                    self.screen = Screen::Dash;
                }
                self.scan_done = true;
            }
            Err(error) => {
                // Do not hide data merely because a background scan hit a
                // temporary error; the next interval will retry.
                self.status = format!("Refresh failed; keeping last verified view: {error}");
                self.refresh_interval = self.next_refresh_interval(true);
                if !matches!(self.screen, Screen::Fading) {
                    self.pending_clean = None;
                    self.skipped_clean_rules.clear();
                }
                if matches!(self.screen, Screen::Reconciling) {
                    self.screen = Screen::Dash;
                }
                // The first scan must still leave the loading screen; an
                // empty-but-explicitly-stale dashboard is recoverable.
                self.scan_done = true;
            }
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
            self.begin_fade(items);
        }
        if matches!(self.screen, Screen::Fading)
            && self
                .fade_started
                .is_some_and(|started| now.duration_since(started) >= FADE_DURATION)
        {
            if let Some(items) = self.pending_clean.clone() {
                self.run_clean(items);
            } else {
                // Nothing to run the fade led up to (e.g. it was cleared out
                // from under us) - land back on the dashboard instead of
                // sitting on `Fading` forever with no way out but quitting.
                self.fade_started = None;
                self.screen = Screen::Dash;
                self.status = "Cleanup did not start; nothing was deleted.".into();
            }
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
    // Keep trackpad and mouse-wheel scrolling inside the alternate screen.
    // Without mouse capture, terminals commonly switch their viewport to the
    // primary buffer, exposing the shell's scrollback behind the dashboard.
    let _ = execute!(stdout(), EnableMouseCapture);
    let result = run_app(&mut terminal);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
    let mut app = App::new();
    // Terminal size changes only on resize events, so retain it instead of
    // querying the backend every marquee frame.
    let mut terminal_width = terminal.size()?.width as usize;
    // Best-effort keyboard-enhancement for key-up/release detection.
    // Falls back to the 150ms key-repeat silence heuristic in hold.rs.
    let _ = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    );
    let (tx, rx) = mpsc::channel::<anyhow::Result<Inventory>>();
    let (clean_tx, clean_rx) = mpsc::channel::<anyhow::Result<execute::ExecReport>>();
    app.clean_tx = Some(clean_tx);
    start_refresh(&mut app, &tx);

    let mut last = Instant::now();
    loop {
        let mut redraw = false;
        // A confirmation is a snapshot of exactly what the user reviewed.
        // Leave a completed scan queued until it is dismissed or acted on.
        if !matches!(app.overlay, Overlay::Confirm) {
            match rx.try_recv() {
                Ok(result) => {
                    redraw = true;
                    app.refresh_in_flight = false;
                    app.refresh_handle = None;
                    // A cleanup or manual refresh asked for newer data while this
                    // worker was running. Never briefly apply its stale snapshot.
                    if !app.refresh_pending {
                        app.apply_refresh_result(result);
                    }
                    app.next_refresh = Instant::now() + app.refresh_interval;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // A worker that panics drops its Sender clone without ever
                    // sending, and try_recv then stays Empty forever (the
                    // original `tx` above keeps the channel open). Detect that
                    // through the JoinHandle instead of hanging in Scan/Reconciling.
                    if app.refresh_in_flight
                        && app.refresh_handle.as_ref().is_some_and(|h| h.is_finished())
                    {
                        redraw = true;
                        app.refresh_in_flight = false;
                        app.refresh_handle = None;
                        if !app.refresh_pending {
                            app.status =
                                "Refresh failed; keeping last verified view: scan worker crashed."
                                    .into();
                            app.refresh_interval = app.next_refresh_interval(true);
                            if !matches!(app.screen, Screen::Fading) {
                                app.pending_clean = None;
                            }
                            if matches!(app.screen, Screen::Reconciling) {
                                app.screen = Screen::Dash;
                            }
                            app.scan_done = true;
                        }
                        app.next_refresh = Instant::now() + app.refresh_interval;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if app.clean_running {
            match clean_rx.try_recv() {
                Ok(result) => {
                    redraw = true;
                    app.finish_clean(result);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if app.clean_handle.as_ref().is_some_and(|h| h.is_finished()) {
                        redraw = true;
                        app.finish_clean(Err(anyhow::anyhow!("clean worker crashed")));
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    redraw = true;
                    app.finish_clean(Err(anyhow::anyhow!("clean worker crashed")));
                }
            }
        }
        // Idle throttle: once animations settle on the dashboard with no
        // overlay open, stop redrawing at 60fps and wait for input instead.
        let marquee = matches!(app.screen, Screen::Dash)
            && matches!(app.overlay, Overlay::None)
            && app.marquee_active(terminal_width);
        let idle = matches!(app.screen, Screen::Dash)
            && matches!(app.overlay, Overlay::None)
            && app.anims_settled()
            && !marquee;
        if !idle || redraw {
            terminal.draw(|f| draw(f, &app))?;
        }
        // While a clean is running, the worker thread is the one doing the
        // (unpredictable-length) work, so poll input on a short, steady
        // cadence instead of blocking a whole frame on it.
        let timeout = if app.clean_running {
            Duration::from_millis(50)
        } else if marquee {
            MARQUEE_STEP
        } else if idle {
            Duration::from_millis(150)
        } else {
            FRAME.saturating_sub(last.elapsed())
        };
        if event::poll(timeout)? {
            // Holding a key (e.g. space through the delete hold-confirm)
            // makes the terminal queue repeat events faster than a full
            // redraw can keep up with; drawing once per event here let the
            // backlog grow without bound, which is what made the UI look
            // stuck until the key was released. Drain everything already
            // queued and redraw once instead.
            let mut drained = 0usize;
            loop {
                match event::read()? {
                    Event::Key(key) => app.handle_key(key),
                    Event::Resize(width, _) => terminal_width = width as usize,
                    _ => {}
                }
                drained += 1;
                if drained >= 256 || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            terminal.draw(|f| draw(f, &app))?;
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64().min(0.05);
        last = now;
        app.tick(dt, now);
        if app.refresh_due(now) {
            start_refresh(&mut app, &tx);
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// Full storage scans can walk large caches. Keep at most one in flight so a
/// periodic update and a post-cleanup reconciliation never compete for disk.
fn start_refresh(app: &mut App, tx: &mpsc::Sender<anyhow::Result<Inventory>>) {
    if app.refresh_in_flight {
        return;
    }
    app.refresh_in_flight = true;
    app.refresh_pending = false;
    let tx = tx.clone();
    app.refresh_handle = Some(thread::spawn(move || {
        let _ = tx.send(scan::inventory(None));
    }));
}

fn draw(frame: &mut Frame, app: &App) {
    // Paint every cell first. Do not rely on the terminal default background:
    // it may be transparent or an image, which makes gaps between widgets and
    // their text unreadable.
    frame.render_widget(Clear, frame.area());
    frame.render_widget(Block::default().style(theme::canvas()), frame.area());
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
        Overlay::Notice => {
            if let Some(notice) = &app.notice {
                modal::notice(frame, &notice.title, &notice.message, notice.color);
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
    // Scanning is transient feedback, not a dashboard. Keep the motion and
    // information in one centered card so large terminals do not turn seven
    // zero-byte progress bars into visual noise.
    // Scale the card with the terminal. A fixed 84-column box looks tiny in
    // an ultrawide window even though scanning is the only thing on screen.
    let viewport = frame.area();
    let card_w = ((viewport.width as u32 * 3) / 5) as u16;
    let card_h = ((viewport.height as u32 * 2) / 3) as u16;
    let area = modal::centered(
        viewport,
        card_w.clamp(84, 118),
        card_h.clamp(19, 28).min(viewport.height),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" ◌  SCANNING LOCAL STORAGE ")
        .title_style(theme::title())
        .border_style(Style::default().fg(theme::CYAN))
        .title_bottom(Line::from(Span::styled(" please wait ", theme::dim())).centered());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let elapsed = app.started.elapsed().as_millis().saturating_sub(700);
    let pulse = (elapsed / 80) as usize;
    let tools: Vec<&'static str> = adapters::all().iter().map(|a| a.id()).collect();
    let active = ((elapsed / 260) as usize).min(tools.len().saturating_sub(1));
    let active_name = tools.get(active).copied().unwrap_or("storage");
    // The scan lanes deliberately span almost the whole card. They make use
    // of ultrawide terminals without pretending that byte discovery has a
    // meaningful percentage complete.
    let grid_width = inner.width.saturating_sub(12).max(1);
    let grid_x = inner.x + inner.width.saturating_sub(grid_width) / 2;
    let signal_width = grid_width.saturating_sub(48) as usize;
    // The content itself is centered vertically in the larger responsive
    // card, leaving deliberate breathing room rather than dead space below.
    let content_h = 21_u16.min(inner.height);
    let content_y = inner.y + inner.height.saturating_sub(content_h) / 2;

    let heading = Line::from(vec![
        Span::styled(
            format!(" {} ", spinner[pulse % spinner.len()]),
            theme::accent(),
        ),
        Span::styled("Mapping ", theme::fg()),
        Span::styled(active_name, theme::accent().add_modifier(Modifier::BOLD)),
        Span::styled(" storage", theme::fg()),
    ]);
    frame.render_widget(
        Paragraph::new(heading).centered(),
        Rect::new(inner.x, content_y, inner.width, 1),
    );

    // A small travelling signal is much easier to read than a fake progress
    // percentage. It continues to move while filesystem discovery is running.
    let track_width = inner.width.saturating_sub(16) as usize;
    let beam = pulse % track_width.max(1);
    let mut sweep = Vec::with_capacity(track_width + 2);
    sweep.push(Span::styled("[", theme::dim()));
    for position in 0..track_width {
        let distance = position.abs_diff(beam);
        let style = match distance {
            0 => theme::accent().add_modifier(Modifier::BOLD),
            1 => Style::default().fg(theme::CYAN),
            _ => theme::dim(),
        };
        sweep.push(Span::styled(if distance == 0 { "◆" } else { "─" }, style));
    }
    sweep.push(Span::styled("]", theme::dim()));
    frame.render_widget(
        Paragraph::new(Line::from(sweep)).centered(),
        Rect::new(inner.x, content_y + 2, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("STATE       ", theme::dim().add_modifier(Modifier::BOLD)),
            Span::styled(
                "TOOL                ",
                theme::dim().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "ACTIVITY       SIGNAL",
                theme::dim().add_modifier(Modifier::BOLD),
            ),
        ])),
        Rect::new(grid_x, content_y + 5, grid_width, 1),
    );

    for (i, name) in tools.iter().enumerate() {
        let done = app.scan_done || i < active;
        let scanning = i == active && !app.scan_done;
        let (state, activity, style) = if done {
            (
                "✓ DONE",
                "INDEXED",
                Style::default()
                    .fg(theme::GREEN)
                    .add_modifier(Modifier::BOLD),
            )
        } else if scanning {
            (
                "◌ ACTIVE",
                "SCANNING",
                theme::accent().add_modifier(Modifier::BOLD),
            )
        } else {
            ("· QUEUED", "WAITING", theme::dim())
        };
        let mut spans = vec![
            Span::styled(format!("{state:<12}"), style),
            Span::styled(
                format!("{name:<20}"),
                theme::fg().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{activity:<12}"), style),
        ];
        for position in 0..signal_width {
            let distance = position.abs_diff((pulse + i * 7) % signal_width.max(1));
            let (glyph, signal_style) = if done {
                ("━", Style::default().fg(theme::GREEN))
            } else if scanning && distance == 0 {
                ("◆", theme::accent().add_modifier(Modifier::BOLD))
            } else if scanning && distance == 1 {
                ("━", Style::default().fg(theme::CYAN))
            } else if scanning {
                ("─", theme::dim())
            } else {
                ("·", theme::dim())
            };
            spans.push(Span::styled(glyph, signal_style));
        }
        let row = Line::from(spans);
        frame.render_widget(
            Paragraph::new(row),
            Rect::new(grid_x, content_y + 7 + i as u16, grid_width, 1),
        );
    }
}

fn draw_cleaning(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // This is deliberately a second, local opaque paint rather than relying
    // on the screen-wide canvas in draw(). During a cleanup the old dashboard
    // must never remain visible between animation frames, even in terminals
    // with transparency, image backgrounds, or aggressive diff rendering.
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme::canvas()), area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" ◈  CLEANING LOCAL STORAGE ")
        .title_style(theme::title())
        .border_style(Style::default().fg(theme::GREEN))
        .title_bottom(
            Line::from(Span::styled(
                " cleanup is running safely in the background ",
                theme::dim(),
            ))
            .centered(),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Actual work happens on a worker thread and its true completion fraction
    // isn't known up front. Instead of a dishonest percentage, show a
    // containment chamber: debris streams inward while the centre core pulses.
    // It is terminal-native take on the particle-dispersion effects popular in
    // Ratatui demos, tuned to AgentSweep's cyan/green system aesthetic.
    let elapsed = app
        .clean_started
        .map(|started| started.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    let phase = (elapsed * 14.0) as usize;
    let content_h = inner.height.min(15);
    let top = inner.y + inner.height.saturating_sub(content_h) / 2;
    let width = inner.width.saturating_sub(8) as usize;
    let left = inner.x + inner.width.saturating_sub(width as u16) / 2;
    let core_width = 19.min(width.saturating_sub(2));
    let core_start = width.saturating_sub(core_width) / 2;

    if width == 0 || content_h < 7 {
        return;
    }

    let status = format!("CONTAINING  {}", trunc(&app.clean_label, 28));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("◌ ", theme::accent().add_modifier(Modifier::BOLD)),
            Span::styled(status, theme::fg().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  ·  {:.0}s elapsed", elapsed), theme::dim()),
        ]))
        .centered(),
        Rect::new(inner.x, top, inner.width, 1),
    );

    // Five independent lanes converge on the core. They suggest real work
    // without claiming that any particular item or byte count is complete.
    let lane_count = content_h.saturating_sub(5).min(7) as usize;
    for lane in 0..lane_count {
        let y = top + 2 + lane as u16;
        let from_left = lane % 2 == 0;
        let distance = (phase + lane * 11) % core_start.max(1);
        let particle = if from_left {
            distance
        } else {
            width.saturating_sub(1 + distance)
        };
        let mut spans = Vec::with_capacity(width);
        for x in 0..width {
            let toward_core = if from_left {
                x
            } else {
                width.saturating_sub(1 + x)
            };
            let (glyph, style) = if x >= core_start && x < core_start + core_width {
                (" ", theme::canvas())
            } else if x == particle {
                ("◆", theme::accent().add_modifier(Modifier::BOLD))
            } else if toward_core < distance && (x + lane + phase / 2) % 5 == 0 {
                ("·", Style::default().fg(theme::GREEN).bg(theme::BG))
            } else if toward_core < distance {
                ("─", Style::default().fg(theme::GREY).bg(theme::BG))
            } else {
                (" ", theme::canvas())
            };
            spans.push(Span::styled(glyph, style));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(left, y, width as u16, 1),
        );
    }

    let pulse = ((elapsed * 5.0).sin() + 1.0) * 0.5;
    let core = if pulse > 0.72 {
        "✦"
    } else if pulse > 0.34 {
        "◈"
    } else {
        "◇"
    };
    let core_color = if pulse > 0.55 {
        theme::GREEN
    } else {
        theme::CYAN
    };
    let core_y = top + 2 + lane_count as u16 / 2;
    let core_label = format!("[ {core}  PURGING  {core} ]");
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            core_label,
            Style::default()
                .fg(core_color)
                .bg(theme::BG)
                .add_modifier(Modifier::BOLD),
        )))
        .centered(),
        Rect::new(left + core_start as u16, core_y, core_width as u16, 1),
    );

    let foot_y = top + content_h.saturating_sub(2);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "INBOUND STREAMS  ·  QUARANTINE WHEN AVAILABLE  ·  DO NOT CLOSE",
            theme::dim(),
        )))
        .centered(),
        Rect::new(inner.x, foot_y, inner.width, 1),
    );
}

fn draw_dash(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(6),
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
        .title(" storage by source ")
        .title_style(theme::title())
        .title_bottom(
            Line::from(Span::styled(
                " ← → switch source  ·  live sources pulse ",
                theme::dim(),
            ))
            .right_aligned(),
        )
        .border_style(Style::default().fg(theme::GREY));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let vis = app.visible_tools();
    let visible_rows = inner.height as usize;
    let start = scroll_start(app.selected_tool, vis.len(), visible_rows);
    for (row, &idx) in vis.iter().enumerate().skip(start).take(visible_rows) {
        let tool = &app.inventory.tools[idx];
        let selected = row == app.selected_tool;
        let y = inner.y + (row - start) as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let label_width = 36.min(inner.width) as usize;
        let runner_width = usize::from(tool.running) * 9;
        let name_width = label_width.saturating_sub(12 + runner_width);
        let marker = if selected { "▌ " } else { "  " };
        let marker_style = if selected {
            theme::accent().add_modifier(Modifier::BOLD)
        } else {
            theme::canvas()
        };
        let name_style = if selected {
            theme::accent().add_modifier(Modifier::BOLD)
        } else {
            theme::fg()
        };
        let mut spans = vec![
            Span::styled(marker, marker_style),
            Span::styled(
                format!("{:<name_width$}", trunc(&tool.id, name_width)),
                name_style,
            ),
        ];
        if tool.running {
            // Alternate the runner's arms in place instead of sliding a
            // picture through the bar, so the source name itself feels live.
            let runner = ["ᕕ(•‿•)ᕗ", "ᕗ(•‿•)ᕕ"];
            let frame = (app.started.elapsed().as_millis() / 140) as usize;
            spans.push(Span::styled(
                format!(" {}", runner[frame % runner.len()]),
                Style::default()
                    .fg(theme::GREEN)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(
            format!("{:>10}", util::bytes(tool.total_bytes())),
            theme::fg().add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(inner.x, y, label_width as u16, 1),
        );
        if inner.width > label_width as u16 + 2 {
            let bar_area = Rect::new(
                inner.x + label_width as u16 + 2,
                y,
                inner.width.saturating_sub(label_width as u16 + 2),
                1,
            );
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
            spans.push(Span::styled("█".repeat(w), Style::default().fg(*color)));
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
            let location = match item.paths.len() {
                0 => String::new(),
                1 => path,
                count => format!("{path}  + {} related locations", count - 1),
            };
            let body = if location.is_empty() {
                trunc(&item.consequence, w)
            } else {
                trunc(&format!("{}  —  {location}", item.consequence), w)
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
    let pending_ids: HashSet<&str> = if matches!(app.screen, Screen::Reconciling) {
        app.pending_clean
            .as_ref()
            .map(|items| items.iter().map(|item| item.rule_id.as_str()).collect())
            .unwrap_or_default()
    } else {
        HashSet::new()
    };
    // Once the dissolve completes, remove finished rows from layout at once.
    // The remaining rows compact into place while reconciliation runs.
    let items: Vec<&Item> = app
        .current_items()
        .into_iter()
        .filter(|item| !pending_ids.contains(item.rule_id.as_str()))
        .collect();
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
    let name_width = item_name_width(&items, inner.width as usize);
    let description_width = item_description_width(&items, inner.width as usize);
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
        let fading = app
            .pending_clean
            .as_ref()
            .is_some_and(|items| items.iter().any(|pending| pending.rule_id == item.rule_id));
        let fade = app.fade_progress().filter(|_| fading);
        let hl = i == app.selected_item && fade.is_none();
        let mut style = if item.risk.locked() {
            theme::dim()
        } else {
            Style::default().fg(theme::risk_color(item.risk))
        };
        if hl {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let description = if hl {
            marquee(
                &item.consequence,
                description_width,
                app.marquee_started.elapsed(),
            )
        } else {
            trunc(&item.consequence, description_width)
        };
        let line = format!(
            "{glyph} {:<name_width$} {:>10}  {:<10}  {description}",
            trunc(&item.label, name_width),
            util::bytes(item.bytes),
            item.risk.label(),
        );
        let line = fade
            .map(|progress| digital_fade(&line, progress, i))
            .unwrap_or(line);
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

/// Give names enough room to identify the source while reserving at least
/// half the row for the operational explanation. A fixed 26-column name
/// column wasted wide terminals and obscured meaningful folder names.
fn item_name_width(items: &[&Item], row_width: usize) -> usize {
    let longest = items
        .iter()
        .map(|item| item.label.chars().count())
        .max()
        .unwrap_or(0);
    let available = row_width.saturating_sub(27);
    longest.min(available / 2).clamp(18, 52).min(available)
}

fn item_description_width(items: &[&Item], row_width: usize) -> usize {
    row_width.saturating_sub(item_name_width(items, row_width) + 27)
}

/// Scroll an overflowing active description after a short reading pause. The
/// inactive rows remain still, so the list stays easy to scan.
fn marquee(text: &str, width: usize, elapsed: Duration) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let span = chars.len() + MARQUEE_SEPARATOR.len();
    let pause_ms = MARQUEE_PAUSE.as_millis();
    let scroll_ms = span as u128 * MARQUEE_STEP.as_millis();
    let cycle_ms = pause_ms * 2 + scroll_ms;
    let phase = elapsed.as_millis() % cycle_ms;
    // Hold the readable beginning both before and after the scroll. The
    // separator is visible only while travelling between the two copies.
    let offset = if phase < pause_ms || phase >= pause_ms + scroll_ms {
        0
    } else {
        ((phase - pause_ms) / MARQUEE_STEP.as_millis()) as usize
    };
    (0..width)
        .map(|column| {
            let source = (offset + column) % span;
            if source < chars.len() {
                chars[source]
            } else {
                MARQUEE_SEPARATOR[source - chars.len()]
            }
        })
        .collect()
}

/// A deterministic dissolve reads as intentional rather than visual noise.
/// Each character turns into a terminal-era block before the final fifth of
/// the transition, when the whole row is guaranteed blank.
fn digital_fade(text: &str, progress: f64, row: usize) -> String {
    if progress >= 0.8 {
        return " ".repeat(text.chars().count());
    }
    let dissolve = (progress / 0.8).clamp(0.0, 1.0);
    text.chars()
        .enumerate()
        .map(|(column, ch)| {
            if ch == ' ' {
                return ch;
            }
            let signal = ((column * 37 + row * 17) % 101) as f64 / 100.0;
            if signal >= dissolve {
                ch
            } else if (column + row).is_multiple_of(3) {
                '░'
            } else {
                '·'
            }
        })
        .collect()
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
            ("enter", "finder"),
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
    use ratatui::style::Color;
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
        assert!(text.contains("storage by source"));
        assert!(text.contains("reclaimable"), "footer renders");
        for word in [
            "toggle", "select", "clean", "restore", "optimize", "help", "quit",
        ] {
            assert!(text.contains(word), "footer shortcut shows {word}");
        }
    }

    #[test]
    fn item_names_expand_on_wide_rows_without_starving_descriptions() {
        let mut app = test_app();
        app.inventory.tools[0].items[0].label = "x".repeat(60);
        let items = app.current_items();
        assert_eq!(item_name_width(&items, 220), 52);
        assert_eq!(item_name_width(&items, 80), 26);
    }

    #[test]
    fn marquee_pauses_then_moves_overflowing_text() {
        let text = "A deliberately long description that needs to move";
        assert_eq!(marquee(text, 12, Duration::ZERO), "A deliberate");
        assert_ne!(
            marquee(text, 12, MARQUEE_PAUSE + MARQUEE_STEP * 3),
            "A deliberate"
        );
        assert!(marquee(
            text,
            12,
            MARQUEE_PAUSE + MARQUEE_STEP * text.chars().count() as u32
        )
        .contains('↺'));

        let full_scroll =
            MARQUEE_STEP * (text.chars().count() as u32 + MARQUEE_SEPARATOR.len() as u32);
        let restart = MARQUEE_PAUSE + full_scroll;
        assert_eq!(marquee(text, 12, restart), "A deliberate");
        assert_eq!(
            marquee(text, 12, restart + MARQUEE_PAUSE - Duration::from_millis(1)),
            "A deliberate"
        );
    }

    #[test]
    fn dashboard_paints_an_opaque_background_in_every_cell() {
        let app = test_app();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();

        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .all(|cell| cell.bg != Color::Reset),
            "every cell needs an explicit background so terminal transparency cannot show through"
        );
    }

    #[test]
    fn running_tool_keeps_the_dashboard_animating() {
        let mut app = test_app();
        app.inventory.tools[0].running = true;

        let text = rendered(&app, 120, 30);
        assert!(text.contains("ᕕ") || text.contains("ᕗ"));
        assert!(
            !app.anims_settled(),
            "a running source must keep the runner redraw loop active"
        );
    }

    #[test]
    fn cleaning_chamber_replaces_the_previous_dashboard_frame() {
        let mut app = test_app();
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();

        app.screen = Screen::Cleaning;
        app.clean_running = true;
        app.clean_started = Some(Instant::now() - Duration::from_secs(3));
        app.clean_label = "Temporary files".into();
        terminal.draw(|f| draw(f, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("CLEANING LOCAL STORAGE"));
        assert!(text.contains("PURGING"));
        assert!(
            !text.contains("Old plan files"),
            "a cleaning frame must erase dashboard labels before drawing its animation"
        );
        assert!(
            buffer.content().iter().all(|cell| cell.bg != Color::Reset),
            "the cleaning chamber must stay opaque in transparent terminals"
        );
    }

    #[test]
    fn scan_is_a_compact_centered_activity_card() {
        let mut app = App::new();
        app.screen = Screen::Scan;
        let text = rendered(&app, 120, 36);

        assert!(text.contains("SCANNING LOCAL STORAGE"));
        assert!(text.contains("Mapping"));
        assert!(text.contains("SCANNING"));
        assert!(text.contains("QUEUED"));
        assert!(
            text.contains("◆"),
            "the scan should show a moving sweep signal"
        );
        assert!(
            !text.contains("0 B"),
            "an in-progress scan must not present empty sizes as progress"
        );
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

    #[test]
    fn select_allowed_toggles_currently_applicable_items() {
        let mut app = test_app();

        app.select_allowed();
        assert!(app.selected.contains("claude.plans"));
        assert_eq!(app.anim_reclaim.target(), 143_360.0);

        app.select_allowed();
        assert!(app.selected.is_empty());
        assert_eq!(app.anim_reclaim.target(), 0.0);
    }

    #[test]
    fn digital_fade_replaces_visible_characters_with_retro_blocks() {
        let faded = digital_fade("selected cache row", 0.7, 3);
        assert_ne!(faded, "selected cache row");
        assert!(faded.contains('░') || faded.contains('·'));
        assert_eq!(digital_fade("selected cache row", 0.8, 3), " ".repeat(18));
        assert_eq!(digital_fade("abc", 0.0, 0), "abc");
    }

    #[test]
    fn escape_cancels_the_pre_cleanup_fade() {
        let mut app = test_app();
        app.begin_fade(app.selected_items());

        app.handle_key(KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert!(matches!(app.screen, Screen::Dash));
        assert!(app.pending_clean.is_none());
        assert_eq!(app.status, "Cleanup cancelled.");
    }

    #[test]
    fn a_scan_landing_during_the_pre_cleanup_fade_does_not_drop_the_clean() {
        // Reproduces a real hang: a scan already in flight when the
        // hold-confirm opened can land just after the hold confirms, while
        // `Fading` is still holding `pending_clean` for a clean that has not
        // started yet. `apply_refresh_result` used to null `pending_clean`
        // unconditionally, so by the time the fade timer elapsed there was
        // nothing left to run - `Fading` never advanced to `Cleaning` and
        // just sat there, indistinguishable from a frozen UI, until the user
        // gave up and pressed `q`.
        let mut app = test_app();
        let (clean_tx, _clean_rx) = mpsc::channel();
        app.clean_tx = Some(clean_tx);
        let items = app.selected_items();
        app.begin_fade(items);
        assert!(matches!(app.screen, Screen::Fading));

        app.apply_refresh_result(Ok(app.inventory.clone()));

        assert!(
            app.pending_clean.is_some(),
            "a scan landing mid-fade must not cancel the pending clean"
        );
        assert!(matches!(app.screen, Screen::Fading));

        let now = Instant::now();
        app.fade_started = Some(now - FADE_DURATION);
        app.tick(0.0, now);

        assert!(
            matches!(app.screen, Screen::Cleaning),
            "the fade must still hand off to the real clean once its timer elapses"
        );
    }

    #[test]
    fn fading_with_nothing_pending_lands_back_on_dash_instead_of_hanging() {
        let mut app = test_app();
        app.screen = Screen::Fading;
        app.fade_started = Some(Instant::now() - FADE_DURATION);
        app.pending_clean = None;

        app.tick(0.0, Instant::now());

        assert!(matches!(app.screen, Screen::Dash));
        assert!(app.fade_started.is_none());
    }

    #[test]
    fn quit_is_refused_while_a_clean_is_actually_running() {
        // Once execute() has started moving/deleting files there is no safe
        // rollback point, so quitting here (unlike during the pre-clean fade)
        // must not tear down the process out from under the worker thread.
        let mut app = test_app();
        app.screen = Screen::Cleaning;
        app.clean_running = true;

        app.handle_key(KeyEvent::new(
            KeyCode::Char('q'),
            crossterm::event::KeyModifiers::NONE,
        ));

        assert!(matches!(app.screen, Screen::Cleaning));
        assert!(!app.should_quit);
        assert_eq!(
            app.status,
            "Cleanup in progress; please wait for it to finish."
        );
    }

    #[test]
    fn inventory_refresh_reconciles_selection_without_losing_navigation() {
        let mut app = test_app();
        app.selected.insert("claude.plans".into());
        let mut refreshed = app.inventory.clone();
        refreshed.tools[0].items[0].bytes = 8192;

        app.apply_inventory(refreshed.clone());
        assert!(app.selected.contains("claude.plans"));
        assert_eq!(app.current_tool_id(), "claude");
        assert_eq!(app.current_items()[0].bytes, 8192);

        refreshed.tools[0].items.clear();
        app.apply_inventory(refreshed);
        assert!(app.selected.is_empty(), "gone items cannot stay selected");
        assert_eq!(app.selected_item, 0, "selection is clamped after refresh");
    }

    #[test]
    fn reappearance_after_clean_is_detected_from_a_fresh_scan() {
        // A move can report success and still not stick if something outside
        // AgentSweep's control - another running tool sharing the same
        // on-disk storage - recreates the path a moment later. This is what
        // actually happened with a real Windsurf/Cascade `code_tracker`
        // directory: the quarantine succeeded, but a fresh scan moments
        // later still found real bytes at the same rule_id.
        let mut app = test_app();
        let cleaned = app.inventory.tools[0].items.clone();
        app.pending_clean = Some(cleaned);

        let mut still_present = app.inventory.clone();
        still_present.tools[0].items[0].bytes = 4096;
        assert_eq!(
            app.reappeared_after_clean(&still_present),
            vec!["Old plan files".to_string()]
        );

        // A running tool safety skip is intentional, not a cleanup that
        // claimed success and then reappeared.
        app.skipped_clean_rules.insert("claude.plans".into());
        assert!(app.reappeared_after_clean(&still_present).is_empty());
        app.skipped_clean_rules.clear();

        let mut actually_gone = app.inventory.clone();
        actually_gone.tools[0].items.clear();
        assert!(app.reappeared_after_clean(&actually_gone).is_empty());
    }

    #[test]
    fn refresh_backoff_doubles_when_unchanged_and_caps() {
        let mut app = test_app();
        assert_eq!(app.refresh_interval, REFRESH_INTERVAL);
        app.refresh_interval = app.next_refresh_interval(false);
        assert_eq!(app.refresh_interval, REFRESH_INTERVAL * 2);
        app.refresh_interval = app.next_refresh_interval(false);
        assert_eq!(app.refresh_interval, REFRESH_INTERVAL * 4);
        for _ in 0..10 {
            app.refresh_interval = app.next_refresh_interval(false);
        }
        assert_eq!(
            app.refresh_interval, MAX_REFRESH_INTERVAL,
            "backoff must not grow without bound"
        );
    }

    #[test]
    fn refresh_backoff_resets_on_any_change() {
        let mut app = test_app();
        app.refresh_interval = MAX_REFRESH_INTERVAL;
        assert_eq!(app.next_refresh_interval(true), REFRESH_INTERVAL);
    }

    #[test]
    fn opening_an_item_without_a_path_explains_why() {
        let mut app = test_app();
        app.inventory.tools[0].items[0].paths.clear();

        app.open_current_item();

        assert_eq!(app.status, "No path available for Old plan files.");
    }
}
