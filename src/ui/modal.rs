use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::model::Item;
use crate::ui::theme;
use crate::util;

pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w.min(area.width), h.min(area.height))
}

/// Covers the modal rectangle with the same opaque surface as the dashboard.
/// `Clear` resets cells to the terminal default style, which leaks through a
/// transparent or image-backed terminal precisely where the modal needs the
/// most contrast.
fn opaque_surface(frame: &mut Frame, area: Rect) {
    // Styling alone retains the previously-rendered glyphs. Clear them before
    // painting the opaque layer so panels never show dashboard text beneath.
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme::canvas()), area);
}

pub fn confirm_modal(frame: &mut Frame, items: &[Item], progress: f64, userdata: bool) {
    let border = theme::amber_to_red(progress);
    let mut lines: Vec<Line> = Vec::new();
    for item in items.iter().take(6) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<38}", truncate(&item.label, 38)),
                Style::default().fg(theme::FG),
            ),
            Span::styled(
                format!("{:>10}", util::bytes(item.bytes)),
                Style::default().fg(theme::risk_color(item.risk)),
            ),
        ]));
    }
    if items.len() > 6 {
        lines.push(Line::from(Span::styled(
            format!("… {} more", items.len() - 6),
            theme::dim(),
        )));
    }
    for item in items
        .iter()
        .filter(|i| i.risk != crate::model::Risk::Safe)
        .take(3)
    {
        lines.push(Line::from(Span::styled(
            format!("✗  {}", truncate(&item.consequence, 52)),
            Style::default().fg(theme::ORANGE),
        )));
    }
    lines.push(Line::from(Span::styled(
        "✓  recoverable from quarantine for 7 days",
        Style::default().fg(theme::GREEN),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("      "),
        Span::styled(meter(progress, 28), Style::default().fg(border)),
        Span::styled(format!("  {:>3}%", (progress * 100.0) as u16), theme::fg()),
    ]));
    lines.push(Line::from(Span::styled(
        "        HOLD SPACE TO CONFIRM",
        Style::default().fg(border).add_modifier(Modifier::BOLD),
    )));
    // One top padding row plus borders. A single-item review becomes a focused
    // card, while multi-item batches get only the rows they actually need.
    let h = (lines.len() as u16 + 3)
        .clamp(9, 16)
        .min(frame.area().height);
    let area = centered(frame.area(), 62, h);
    opaque_surface(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(if userdata {
            " ⚠  THIS IS YOUR DATA, NOT A CACHE "
        } else {
            " ⚠  REVIEW BEFORE DELETING "
        })
        .title_style(Style::default().fg(border).add_modifier(Modifier::BOLD))
        .title_bottom(
            Line::from(vec![
                Span::styled(" esc ", theme::keycap()),
                Span::styled(" cancel ", theme::dim()),
            ])
            .right_aligned(),
        )
        .padding(Padding::new(2, 2, 1, 0));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

pub fn refused_modal(frame: &mut Frame, item: &Item) {
    // Refusal is an acknowledgement, not a long-form confirmation. Keep it
    // deliberately compact so it reads as a focused safety boundary instead
    // of a mostly empty dialog.
    let area = centered(frame.area(), 64, 8.min(frame.area().height));
    opaque_surface(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::RED))
        .title(" 🔒  REFUSED ")
        .title_style(Style::default().fg(theme::RED).add_modifier(Modifier::BOLD))
        .title_bottom(
            Line::from(vec![
                Span::styled(" esc ", theme::keycap()),
                Span::styled(" close ", theme::dim()),
            ])
            .right_aligned(),
        )
        .padding(Padding::new(2, 2, 1, 0));
    let path = item
        .paths
        .first()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let lines = vec![
        Line::from(Span::styled(
            item.label.clone(),
            theme::fg().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(truncate(&path, 56), theme::dim())),
        Line::from(""),
        Line::from(Span::styled(
            "Protected: auth, configuration, memories, and plugins are never deleted.",
            Style::default().fg(theme::RED),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

/// A bounded, dismissible explanation for recoverable action failures. Keeping
/// OS diagnostics out of the terminal prevents them from corrupting the TUI or
/// filling the user's scrollback.
pub fn notice(frame: &mut Frame, title: &str, message: &str, color: Color) {
    let area = centered(frame.area(), 68, 9.min(frame.area().height));
    opaque_surface(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(title)
        .title_style(Style::default().fg(color).add_modifier(Modifier::BOLD));
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {message}"), theme::fg())),
        Line::from(""),
        Line::from(Span::styled(
            "                 Press any key to close",
            theme::dim(),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

pub fn pick_list(frame: &mut Frame, title: &str, options: &[String], selected: usize) {
    let h = (options.len() as u16 + 4)
        .min(frame.area().height.saturating_sub(2))
        .max(6);
    let area = centered(frame.area(), 64, h);
    opaque_surface(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::CYAN))
        .title(format!(" {title} "))
        .title_style(theme::title());
    let lines: Vec<Line> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let prefix = if i == selected { " ▶ " } else { "   " };
            let style = if i == selected {
                Style::default()
                    .fg(theme::CYAN)
                    .add_modifier(Modifier::BOLD)
            } else {
                theme::fg()
            };
            Line::from(Span::styled(format!("{prefix}{opt}"), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left),
        area,
    );
}

pub fn help_overlay(frame: &mut Frame) {
    // A reference card is scanned, not read top-to-bottom. Split the controls
    // into two columns so it stays compact even on tall terminals.
    let area = centered(frame.area(), 78, 14.min(frame.area().height));
    opaque_surface(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::CYAN))
        .title(" shortcuts ")
        .title_style(theme::title())
        .title_bottom(
            Line::from(vec![
                Span::styled(" esc ", theme::keycap()),
                Span::styled(" close ", theme::dim()),
            ])
            .right_aligned(),
        );
    let inner_w = area.width.saturating_sub(4) as usize;
    let col_w = inner_w.saturating_sub(2) / 2;
    let left = vec![
        section("navigate", col_w),
        compact_help_row("↑ ↓", vec![Span::styled("move item", theme::fg())]),
        compact_help_row("← →", vec![Span::styled("switch tool", theme::fg())]),
        compact_help_row(
            "tab",
            vec![
                Span::styled("mode ", theme::fg()),
                Span::styled("SAFE", theme::accent().add_modifier(Modifier::BOLD)),
                Span::styled(" · ", theme::dim()),
                Span::styled("SMART", theme::accent().add_modifier(Modifier::BOLD)),
                Span::styled(" · ", theme::dim()),
                Span::styled("DEEP", theme::accent().add_modifier(Modifier::BOLD)),
            ],
        ),
        compact_help_row(
            "o",
            vec![
                Span::styled("age ", theme::fg()),
                Span::styled("All · >7d · >30d · >90d", theme::accent()),
            ],
        ),
        Line::from(""),
        section("select", col_w),
        compact_help_row("space", vec![Span::styled("toggle item", theme::fg())]),
        compact_help_row("enter", vec![Span::styled("show in Finder", theme::fg())]),
        compact_help_row("a", vec![Span::styled("select allowed", theme::fg())]),
        compact_help_row("n", vec![Span::styled("clear selection", theme::fg())]),
    ];
    let right = vec![
        section("act", col_w),
        compact_help_row("d", vec![Span::styled("clean selection", theme::fg())]),
        compact_help_row("r", vec![Span::styled("restore quarantine", theme::fg())]),
        compact_help_row(
            "p",
            vec![Span::styled("prevention / optimize", theme::fg())],
        ),
        compact_help_row("?", vec![Span::styled("this help", theme::fg())]),
        compact_help_row("q esc", vec![Span::styled("quit / close", theme::fg())]),
    ];
    let content_h = area.height.saturating_sub(2);
    let y = area.y + 1;
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(left),
        Rect::new(area.x + 2, y, col_w as u16, content_h),
    );
    frame.render_widget(
        Paragraph::new(right),
        Rect::new(area.x + 2 + col_w as u16 + 2, y, col_w as u16, content_h),
    );
}

fn section(name: &'static str, inner_w: usize) -> Line<'static> {
    let label = format!(" {name} ");
    let rule = "─".repeat(inner_w.saturating_sub(label.chars().count()));
    Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(theme::MAGENTA)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(rule, theme::dim()),
    ])
}

fn compact_help_row(key: &str, desc: Vec<Span<'static>>) -> Line<'static> {
    const COL: usize = 8;
    let cap = format!(" {key} ");
    let pad = COL.saturating_sub(cap.chars().count()).max(1);
    let mut spans = vec![
        Span::styled(cap, theme::keycap()),
        Span::raw(" ".repeat(pad)),
    ];
    spans.extend(desc);
    Line::from(spans)
}

fn meter(progress: f64, width: usize) -> String {
    let filled = ((progress * width as f64).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let t: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{t}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_help(w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(help_overlay).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn help_groups_keys_and_labels() {
        let text = render_help(80, 24);
        for needle in [
            "shortcuts",
            "navigate",
            "select",
            "act",
            "toggle item",
            "clean selection",
            "close",
        ] {
            assert!(text.contains(needle), "help should show {needle}");
        }
    }

    #[test]
    fn help_panel_clears_the_dashboard_beneath_it() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let background = (0..24)
                    .map(|_| Line::from("X".repeat(80)))
                    .collect::<Vec<_>>();
                frame.render_widget(Paragraph::new(background), frame.area());
                help_overlay(frame);
            })
            .unwrap();

        let area = centered(Rect::new(0, 0, 80, 24), 78, 14);
        let buffer = terminal.backend().buffer();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                assert_ne!(
                    buffer[(x, y)].symbol(),
                    "X",
                    "the modal must not retain characters from the dashboard"
                );
            }
        }
    }
}
