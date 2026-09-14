use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
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

pub fn confirm_modal(frame: &mut Frame, items: &[Item], progress: f64, userdata: bool) {
    let area = centered(frame.area(), 62, 16.min(frame.area().height));
    frame.render_widget(Clear, area);
    let border = theme::amber_to_red(progress);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(if userdata {
            " ⚠  THIS IS YOUR DATA, NOT A CACHE "
        } else {
            " ⚠  REVIEW BEFORE DELETING "
        })
        .title_style(Style::default().fg(border).add_modifier(Modifier::BOLD));

    let mut lines: Vec<Line> = vec![Line::from("")];
    for item in items.iter().take(6) {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<28}", truncate(&item.label, 28)),
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
            format!("  … {} more", items.len() - 6),
            theme::dim(),
        )));
    }
    lines.push(Line::from(""));
    for item in items
        .iter()
        .filter(|i| i.risk != crate::model::Risk::Safe)
        .take(3)
    {
        lines.push(Line::from(Span::styled(
            format!("  ✗  {}", truncate(&item.consequence, 50)),
            Style::default().fg(theme::ORANGE),
        )));
    }
    lines.push(Line::from(Span::styled(
        "  ✓  recoverable from quarantine for 7 days",
        Style::default().fg(theme::GREEN),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("       "),
        Span::styled(meter(progress, 28), Style::default().fg(border)),
        Span::styled(format!("  {:>3}%", (progress * 100.0) as u16), theme::fg()),
    ]));
    lines.push(Line::from(Span::styled(
        "           HOLD SPACE TO CONFIRM",
        Style::default().fg(border).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "              Esc to cancel",
        theme::dim(),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

pub fn refused_modal(frame: &mut Frame, item: &Item) {
    let area = centered(frame.area(), 56, 12.min(frame.area().height));
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::RED))
        .title(" 🔒  REFUSED ")
        .title_style(Style::default().fg(theme::RED).add_modifier(Modifier::BOLD));
    let path = item
        .paths
        .first()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {}", item.label), theme::fg())),
        Line::from(Span::styled(
            format!("  {}", truncate(&path, 50)),
            theme::dim(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  AgentSweep will never delete auth,",
            Style::default().fg(theme::RED),
        )),
        Line::from(Span::styled(
            "  configuration, memories, or plugins.",
            Style::default().fg(theme::RED),
        )),
        Line::from(""),
        Line::from(Span::styled("                  [ OK ]", theme::accent())),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub fn pick_list(frame: &mut Frame, title: &str, options: &[String], selected: usize) {
    let h = (options.len() as u16 + 4)
        .min(frame.area().height.saturating_sub(2))
        .max(6);
    let area = centered(frame.area(), 64, h);
    frame.render_widget(Clear, area);
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
    let area = centered(frame.area(), 58, 20.min(frame.area().height));
    frame.render_widget(Clear, area);
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
        )
        .padding(Padding::new(1, 1, 1, 0));
    let inner_w = block.inner(area).width as usize;

    let lines = vec![
        section("navigate", inner_w),
        help_row(
            "↑ ↓",
            vec![Span::styled("move within the item list", theme::fg())],
        ),
        help_row("← →", vec![Span::styled("switch tool", theme::fg())]),
        help_row(
            "tab",
            vec![
                Span::styled("cycle mode  ", theme::fg()),
                Span::styled("SAFE", theme::accent().add_modifier(Modifier::BOLD)),
                Span::styled(" · ", theme::dim()),
                Span::styled("SMART", theme::accent().add_modifier(Modifier::BOLD)),
                Span::styled(" · ", theme::dim()),
                Span::styled("DEEP", theme::accent().add_modifier(Modifier::BOLD)),
            ],
        ),
        help_row(
            "o",
            vec![
                Span::styled("cycle age   ", theme::fg()),
                Span::styled("All", theme::accent()),
                Span::styled(" · ", theme::dim()),
                Span::styled(">7d", theme::accent()),
                Span::styled(" · ", theme::dim()),
                Span::styled(">30d", theme::accent()),
                Span::styled(" · ", theme::dim()),
                Span::styled(">90d", theme::accent()),
            ],
        ),
        Line::from(""),
        section("select", inner_w),
        help_row(
            "space",
            vec![
                Span::styled("toggle item", theme::fg()),
                Span::styled("  ·  ", theme::dim()),
                Span::styled("locked rows show ", theme::dim()),
                Span::styled(
                    "REFUSED",
                    Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
                ),
            ],
        ),
        help_row(
            "a",
            vec![Span::styled(
                "select everything the mode allows",
                theme::fg(),
            )],
        ),
        help_row("n", vec![Span::styled("clear selection", theme::fg())]),
        Line::from(""),
        section("act", inner_w),
        help_row("d", vec![Span::styled("clean selection", theme::fg())]),
        help_row(
            "r",
            vec![Span::styled("restore from quarantine", theme::fg())],
        ),
        help_row(
            "p",
            vec![Span::styled("prevention / optimize", theme::fg())],
        ),
        help_row("?", vec![Span::styled("this help", theme::fg())]),
        help_row("q Esc", vec![Span::styled("quit / back", theme::fg())]),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
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

fn help_row(key: &str, desc: Vec<Span<'static>>) -> Line<'static> {
    const COL: usize = 10;
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
}
