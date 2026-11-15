//! The Console: every agent as a two-line item (bubbles list's default
//! delegate): title on top, status and place underneath, a fuchsia bar on
//! the selected one.

use super::text::{str_width, wrap};
use super::theme::t;
use super::widgets::{highlight, input_spans, line_lr, truncate_spans};
use super::{location, status_glyph, status_label, status_style};
use crate::app::{App, Filter};
use crate::session::ConnState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect) {
    let th = t();
    let rows = app.rows();
    let gap = if area.height >= 8 { 1 } else { 0 };
    let [status, _, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(gap),
        Constraint::Min(1),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(status_line(app, rows.len())), status);

    if rows.is_empty() {
        let w = (list.width as usize).saturating_sub(2);
        let lines: Vec<Line> = empty_message(app)
            .iter()
            .flat_map(|l| wrap(l, w))
            .map(|l| Line::styled(format!("  {l}"), Style::new().fg(th.muted)))
            .collect();
        f.render_widget(Paragraph::new(lines), list);
        return;
    }

    let width = list.width as usize;
    let multi_host = app.hosts.len() > 1;
    let words: Vec<String> = app
        .console
        .search
        .split_whitespace()
        .map(String::from)
        .collect();
    // A blank line between items while everything fits; packed otherwise.
    let spaced = rows.len() * 3 <= list.height as usize + 1;
    let selected = app.console.selected.min(rows.len() - 1);
    // Program and host as right-hand columns of fixed width.
    let prog_w = rows
        .iter()
        .map(|a| str_width(a.program()))
        .max()
        .unwrap_or(0);
    let host_w = rows
        .iter()
        .map(|a| str_width(&a.key.host))
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let sel = i == selected;
            let live = app.is_live(&a.key);
            let bar = if sel {
                Span::styled("│ ", Style::new().fg(th.accent_dim))
            } else {
                Span::raw("  ")
            };
            let (glyph_style, title_style, desc_style) = if !live {
                let dim = Style::new().fg(th.subtle);
                (dim, dim, dim)
            } else if sel {
                (
                    status_style(a.status),
                    Style::new().fg(th.accent).add_modifier(Modifier::BOLD),
                    Style::new().fg(th.accent_dim),
                )
            } else {
                (
                    status_style(a.status),
                    Style::new(),
                    Style::new().fg(th.muted),
                )
            };
            let mut left = vec![
                bar.clone(),
                Span::styled(format!("{} ", status_glyph(app, a.status)), glyph_style),
            ];
            left.extend(highlight(&a.display_title(), &words, title_style));
            let mut right = vec![Span::styled(
                pad(a.program(), prog_w),
                if live {
                    Style::new().fg(th.indigo)
                } else {
                    desc_style
                },
            )];
            if multi_host {
                right.push(Span::styled(
                    format!("  {}", pad(&a.key.host, host_w)),
                    Style::new().fg(th.muted),
                ));
            }
            let status_label_style = if live {
                Style::new().fg(super::status_color(a.status))
            } else {
                desc_style
            };
            let desc = vec![
                bar,
                Span::raw("  "),
                Span::styled(status_label(a.status), status_label_style),
                Span::styled(format!("  {}", location(a)), desc_style),
            ];
            let mut lines = vec![
                line_lr(left, right, width),
                Line::from(truncate_spans(desc, width)),
            ];
            if spaced && i + 1 < rows.len() {
                lines.push(Line::raw(""));
            }
            ListItem::new(Text::from(lines))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    f.render_stateful_widget(List::new(items), list, &mut state);
}

fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(str_width(s))))
}

/// bubbles list's status bar: the count, then the active filters.
fn status_line(app: &App, shown: usize) -> Line<'static> {
    let th = t();
    let muted = Style::new().fg(th.muted);
    let sep = Span::styled(" • ", Style::new().fg(th.faint));
    let mut v: Vec<Span<'static>> = vec![Span::raw("  ")];
    if app.console.searching {
        v.push(Span::styled(
            "検索 ",
            Style::new().fg(th.green).add_modifier(Modifier::BOLD),
        ));
        v.extend(input_spans(&app.console.search, "", Style::new()));
        v.push(sep);
        v.push(Span::styled(format!("{shown} 件"), muted));
        return Line::from(v);
    }
    let total = app.all_agents().len();
    v.push(Span::styled(
        if shown == total {
            format!("{total} 件")
        } else {
            format!("{shown} / {total} 件")
        },
        muted,
    ));
    let tag = Style::new().fg(th.accent_dim);
    if app.console.filter != Filter::All {
        v.push(sep.clone());
        v.push(Span::styled(app.console.filter.label().to_string(), tag));
    }
    if let Some(h) = &app.console.host_filter {
        v.push(sep.clone());
        v.push(Span::styled(format!("ホスト {h}"), tag));
    }
    if !app.console.search.is_empty() {
        v.push(sep.clone());
        v.push(Span::styled(format!("検索 “{}”", app.console.search), tag));
    }
    Line::from(v)
}

fn empty_message(app: &App) -> Vec<String> {
    if app.hosts.is_empty() {
        return vec![
            "herdr が見つかりません。".into(),
            "このマシンで herdr を起動するか、H → a で別のマシンを追加してください。".into(),
        ];
    }
    let loading = app
        .hosts
        .iter()
        .any(|h| h.inventory.is_none() && !matches!(h.state, ConnState::Failed { .. }));
    if loading {
        return vec![format!("{} エージェントを読み込んでいます…", app.spinner())];
    }
    if !app.console.search.is_empty()
        || app.console.filter != Filter::All
        || app.console.host_filter.is_some()
    {
        return vec!["条件に合うエージェントはいません (Esc で絞り込みを解除)".into()];
    }
    let failed: Vec<String> = app
        .hosts
        .iter()
        .filter_map(|h| h.state.error().map(|e| format!("{}: {}", h.cfg.name, e)))
        .collect();
    let mut v = vec!["動いているエージェントはいません。".into()];
    if !failed.is_empty() {
        v.push(String::new());
        v.extend(failed);
    }
    v
}
