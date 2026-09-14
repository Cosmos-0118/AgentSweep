use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::model::{Item, Risk};
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

/// Full, scrollable category inspector. The dashboard's detail strip is a
/// useful summary; this panel deliberately exposes the exact paths and the
/// cleanup contract before the user opens or selects anything.
pub fn item_details(frame: &mut Frame, item: &Item, path_scroll: usize) {
    let viewport = frame.area();
    let area = centered(
        viewport,
        viewport.width.saturating_sub(6).clamp(66, 104),
        viewport.height.saturating_sub(3).clamp(16, 30),
    );
    opaque_surface(frame, area);
    let border = theme::risk_color(item.risk);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(" ◈  CATEGORY INSPECTOR ")
        .title_style(Style::default().fg(border).add_modifier(Modifier::BOLD))
        .title_bottom(
            Line::from(vec![
                Span::styled(" ↑↓ ", theme::keycap()),
                Span::styled(" paths ", theme::dim()),
                Span::styled(" i esc ", theme::keycap()),
                Span::styled(" close ", theme::dim()),
            ])
            .right_aligned(),
        )
        .padding(Padding::new(2, 2, 1, 0));
    frame.render_widget(block, area);

    let inner = Rect::new(
        area.x + 3,
        area.y + 2,
        area.width.saturating_sub(6),
        area.height.saturating_sub(4),
    );
    let width = inner.width.max(12) as usize;
    let mut header = vec![
        Line::from(Span::styled(
            item.label.clone(),
            theme::fg().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                format!("{}  ", item.risk.label()),
                Style::default().fg(border).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}  ·  {}", util::bytes(item.bytes), cleanup_behavior(item)),
                theme::fg(),
            ),
        ]),
        Line::from(Span::styled(
            format!("Rule: {}", item.rule_id),
            theme::dim(),
        )),
    ];
    if item.requires_stopped {
        header.push(Line::from(Span::styled(
            "Requires Codex to be fully closed before cleanup.",
            Style::default().fg(theme::ORANGE),
        )));
    }
    header.push(Line::from(""));
    header.push(Line::from(Span::styled(
        "WHAT THIS DOES",
        theme::accent().add_modifier(Modifier::BOLD),
    )));
    header.extend(wrapped_lines(&item.consequence, width));
    header.push(Line::from(""));
    header.push(Line::from(Span::styled(
        format!("PATHS ({})", item.paths.len()),
        theme::accent().add_modifier(Modifier::BOLD),
    )));

    let header_height = header.len() as u16;
    let body_y = inner.y.saturating_add(header_height);
    let body_height = inner.height.saturating_sub(header_height);
    frame.render_widget(
        Paragraph::new(header),
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            header_height.min(inner.height),
        ),
    );

    let mut paths = Vec::new();
    for path in &item.paths {
        let text = path.display().to_string();
        let wrapped = wrap_text(&text, width.saturating_sub(3));
        for (index, line) in wrapped.into_iter().enumerate() {
            let prefix = if index == 0 { " • " } else { "   " };
            paths.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(border)),
                Span::styled(
                    line,
                    if index == 0 {
                        theme::fg()
                    } else {
                        theme::dim()
                    },
                ),
            ]));
        }
    }
    if paths.is_empty() {
        paths.push(Line::from(Span::styled(
            "No filesystem path is available for this category.",
            theme::dim(),
        )));
    }
    let max_scroll = paths.len().saturating_sub(body_height as usize);
    let start = path_scroll.min(max_scroll);
    let shown: Vec<Line> = paths
        .into_iter()
        .skip(start)
        .take(body_height as usize)
        .collect();
    frame.render_widget(
        Paragraph::new(shown),
        Rect::new(inner.x, body_y, inner.width, body_height),
    );
}

fn cleanup_behavior(item: &Item) -> &'static str {
    if item.delegate.is_some() {
        return "permanent managed prune";
    }
    match item.risk {
        Risk::Safe => "permanent cleanup",
        Risk::Review => "quarantined for 7 days",
        Risk::Userdata => "DEEP mode · quarantined for 7 days",
        Risk::Critical => "protected · never deleted",
        Risk::Unknown => "unclassified · never deleted",
    }
}

fn wrapped_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    wrap_text(text, width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, theme::fg())))
        .collect()
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let sep = usize::from(!current.is_empty());
        if !current.is_empty() && current.chars().count() + sep + word.chars().count() > width {
            out.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        // Paths and rule IDs may contain no whitespace. Split those chunks
        // rather than letting one long string bleed through the border.
        if current.is_empty() && word.chars().count() > width {
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(width) {
                out.push(chunk.iter().collect());
            }
            continue;
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
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
    let delegated = items.iter().any(|item| item.delegate.is_some());
    lines.push(Line::from(Span::styled(
        if delegated {
            "✗  delegated cleanup is permanent; it cannot be restored"
        } else {
            "✓  recoverable from quarantine for 7 days"
        },
        Style::default().fg(if delegated {
            theme::ORANGE
        } else {
            theme::GREEN
        }),
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
    let viewport = frame.area();
    // This is a focused command guide, not a small tooltip. Paint the entire
    // viewport first so dashboard rows cannot compete with the reference card.
    frame.render_widget(Clear, viewport);
    frame.render_widget(Block::default().style(theme::canvas()), viewport);

    let area = help_area(viewport);
    opaque_surface(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::CYAN))
        .title(" ◈  SHORTCUTS & COMMAND GUIDE ")
        .title_style(theme::title())
        .title_bottom(
            Line::from(vec![
                Span::styled(" esc ", theme::keycap()),
                Span::styled(" close ", theme::dim()),
            ])
            .right_aligned(),
        );
    let inner_w = area.width.saturating_sub(8) as usize;
    let col_w = inner_w.saturating_sub(4) / 2;
    let mut left = vec![
        section("navigate", col_w),
        Line::from(Span::styled(
            "Move through the storage map and change its view.",
            theme::dim(),
        )),
        Line::from(""),
    ];
    left.extend(spaced_help_rows(vec![
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
    ]));
    left.extend([
        Line::from(""),
        section("select", col_w),
        Line::from(Span::styled(
            "Build a cleanup set before you act.",
            theme::dim(),
        )),
        Line::from(""),
    ]);
    left.extend(spaced_help_rows(vec![
        compact_help_row("space", vec![Span::styled("toggle item", theme::fg())]),
        compact_help_row("enter", vec![Span::styled("show in Finder", theme::fg())]),
        compact_help_row("i", vec![Span::styled("inspect category", theme::fg())]),
        compact_help_row(
            "a",
            vec![Span::styled("select / deselect allowed", theme::fg())],
        ),
        compact_help_row("n", vec![Span::styled("clear selection", theme::fg())]),
    ]));
    let mut right = vec![
        section("act", col_w),
        Line::from(Span::styled(
            "Run cleanup, restore a snapshot, or update results.",
            theme::dim(),
        )),
        Line::from(""),
    ];
    right.extend(spaced_help_rows(vec![
        compact_help_row("d", vec![Span::styled("clean selection", theme::fg())]),
        compact_help_row("r", vec![Span::styled("restore quarantine", theme::fg())]),
        compact_help_row(
            "p",
            vec![Span::styled("prevention / optimize", theme::fg())],
        ),
        compact_help_row("u", vec![Span::styled("refresh now", theme::fg())]),
    ]));
    right.extend([
        Line::from(""),
        section("quick help", col_w),
        Line::from(Span::styled(
            "Cleanup is recoverable from quarantine when available.",
            theme::dim(),
        )),
        Line::from(""),
    ]);
    right.extend(spaced_help_rows(vec![
        compact_help_row("?", vec![Span::styled("this help", theme::fg())]),
        compact_help_row("q esc", vec![Span::styled("quit / close", theme::fg())]),
    ]));
    let header = Line::from(vec![
        Span::styled("AGENTSWEEP", theme::accent().add_modifier(Modifier::BOLD)),
        Span::styled("  //  LOCAL STORAGE CONTROL SURFACE", theme::dim()),
    ]);
    let content_h = area.height.saturating_sub(6);
    let y = area.y + 5;
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(header),
        Rect::new(area.x + 4, area.y + 2, area.width.saturating_sub(8), 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Keyboard-first controls for inspecting, selecting, and safely reclaiming space.",
            theme::fg(),
        ))),
        Rect::new(area.x + 4, area.y + 3, area.width.saturating_sub(8), 1),
    );
    frame.render_widget(
        Paragraph::new(left),
        Rect::new(area.x + 4, y, col_w as u16, content_h),
    );
    frame.render_widget(
        Paragraph::new(right),
        Rect::new(area.x + 4 + col_w as u16 + 4, y, col_w as u16, content_h),
    );
}

fn help_area(viewport: Rect) -> Rect {
    let width = viewport.width.saturating_sub(8).clamp(82, 116);
    let height = viewport.height.saturating_sub(6).clamp(19, 26);
    centered(
        viewport,
        width.min(viewport.width),
        height.min(viewport.height),
    )
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

/// Keep highlighted keycaps from touching vertically. A single blank terminal
/// row makes each shortcut read as an individual control instead of a cyan
/// column, particularly in the two-column guide.
fn spaced_help_rows(rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut spaced = Vec::with_capacity(rows.len().saturating_mul(2));
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            spaced.push(Line::from(""));
        }
        spaced.push(row);
    }
    spaced
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
    use crate::model::{Item, Risk};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

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
        let text = render_help(120, 36);
        for needle in [
            "SHORTCUTS",
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

        let area = help_area(Rect::new(0, 0, 80, 24));
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

    #[test]
    fn delegated_cleanup_is_not_presented_as_recoverable() {
        let item = Item {
            rule_id: "opencode.sessions".into(),
            tool: "opencode".into(),
            label: "Sessions inactive >30d".into(),
            paths: vec![PathBuf::from("/tmp/opencode.db")],
            bytes: 1,
            risk: Risk::Userdata,
            requires_stopped: true,
            consequence: "permanent".into(),
            oldest_mtime: None,
            newest_mtime: None,
            delegate: Some("opencode.session_delete".into()),
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| confirm_modal(frame, &[item], 0.0, true))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("permanent"));
        assert!(!text.contains("recoverable from quarantine"));
    }
}
