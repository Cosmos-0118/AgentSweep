use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub const BG: Color = Color::Rgb(12, 14, 20);
pub const FG: Color = Color::Rgb(226, 232, 240);
pub const DIM: Color = Color::Rgb(100, 116, 139);
pub const CYAN: Color = Color::Rgb(34, 211, 238);
pub const MAGENTA: Color = Color::Rgb(232, 121, 249);
pub const GREEN: Color = Color::Rgb(52, 211, 153);
pub const YELLOW: Color = Color::Rgb(251, 191, 36);
pub const ORANGE: Color = Color::Rgb(251, 146, 60);
pub const RED: Color = Color::Rgb(248, 113, 113);
pub const GREY: Color = Color::Rgb(71, 85, 105);

/// Opaque base layer for the whole TUI.
///
/// Many terminal emulators can be configured with a transparent window or a
/// background image. Ratatui's default cells inherit that terminal background,
/// so a dashboard that only paints text and borders becomes hard to read. Draw
/// this style beneath every screen before rendering its content.
pub fn canvas() -> Style {
    Style::default().bg(BG)
}

pub fn risk_color(risk: crate::model::Risk) -> Color {
    match risk {
        crate::model::Risk::Safe => GREEN,
        crate::model::Risk::Review => YELLOW,
        crate::model::Risk::Userdata => ORANGE,
        crate::model::Risk::Critical => RED,
        crate::model::Risk::Unknown => GREY,
    }
}

pub fn title() -> Style {
    Style::default().fg(CYAN).add_modifier(Modifier::BOLD)
}

pub fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn fg() -> Style {
    Style::default().fg(FG)
}

pub fn accent() -> Style {
    Style::default().fg(CYAN)
}

/// Inverted cyan pill used for keyboard shortcuts — same treatment as the
/// active SAFE/SMART/DEEP chip in the header, so keys read as keys.
pub fn keycap() -> Style {
    Style::default()
        .fg(BG)
        .bg(CYAN)
        .add_modifier(Modifier::BOLD)
}

pub fn shortcut(key: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!(" {key} "), keycap()),
        Span::styled(format!(" {label}"), dim()),
    ]
}

/// Gradient from cyan to magenta across `t` in 0..=1.
pub fn gradient(t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let r = lerp(34.0, 232.0, t);
    let g = lerp(211.0, 121.0, t);
    let b = lerp(238.0, 249.0, t);
    Color::Rgb(r as u8, g as u8, b as u8)
}

pub fn amber_to_red(t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let r = lerp(251.0, 248.0, t);
    let g = lerp(191.0, 113.0, t);
    let b = lerp(36.0, 113.0, t);
    Color::Rgb(r as u8, g as u8, b as u8)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
