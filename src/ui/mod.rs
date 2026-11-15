//! Drawing. Pure functions of [`App`] state.
//!
//! The look follows Charm's terminal apps (Bubble Tea / Lip Gloss): a violet
//! title pill, borderless lists with a fuchsia bar on the selected item,
//! quiet gray secondary text and a `key desc • key desc` help line. Colors
//! come from [`theme`].

mod console;
mod detail;
mod help;
mod hosts;
pub mod markdown;
pub mod text;
pub mod theme;
mod usage;
pub mod widgets;

use crate::app::{AddStep, App, Focus, View};
use crate::model::{Agent, AgentStatus, short_path};
use crate::session::ConnState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use theme::t;
use widgets::{help_line, line_lr, pad_spans, pill};

pub use detail::chat_lines;

pub fn status_color(s: AgentStatus) -> Color {
    let th = t();
    match s {
        AgentStatus::Blocked => th.pink,
        AgentStatus::Done => th.green,
        AgentStatus::Working => th.yellow,
        AgentStatus::Idle => th.gray,
        AgentStatus::Unknown => th.subtle,
    }
}

pub fn status_style(s: AgentStatus) -> Style {
    let st = Style::new().fg(status_color(s));
    if s == AgentStatus::Blocked {
        st.add_modifier(Modifier::BOLD)
    } else {
        st
    }
}

pub fn status_label(s: AgentStatus) -> &'static str {
    match s {
        AgentStatus::Blocked => "入力待ち",
        AgentStatus::Done => "完了",
        AgentStatus::Working => "作業中",
        AgentStatus::Idle => "待機",
        AgentStatus::Unknown => "不明",
    }
}

fn status_glyph(app: &App, s: AgentStatus) -> String {
    match s {
        AgentStatus::Working => app.spinner().to_string(),
        _ => status_glyph_static(s).into(),
    }
}

fn status_glyph_static(s: AgentStatus) -> &'static str {
    match s {
        AgentStatus::Blocked => "●",
        AgentStatus::Done => "✔",
        AgentStatus::Working => "◐",
        AgentStatus::Idle => "○",
        AgentStatus::Unknown => "?",
    }
}

fn conn_color(s: &ConnState) -> Color {
    let th = t();
    match s {
        ConnState::Connected => th.green,
        ConnState::Connecting | ConnState::Reconnecting { .. } => th.yellow,
        ConnState::Failed { .. } => th.pink,
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    // Breathing room under the header when there is space for it.
    let gap = if area.height >= 16 { 1 } else { 0 };
    let [header, _, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(gap),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let body = inset(body);
    draw_header(f, app, header);
    match app.view {
        View::Console => console::draw(f, app, body),
        View::Detail => detail::draw(f, app, body),
        View::Hosts => hosts::draw(f, app, body),
        View::Usage => usage::draw(f, app, body),
        View::Help => {
            match app.prev_view {
                View::Detail => detail::draw(f, app, body),
                View::Hosts => hosts::draw(f, app, body),
                View::Usage => usage::draw(f, app, body),
                _ => console::draw(f, app, body),
            }
            help::draw(f, body);
        }
    }
    draw_footer(f, app, footer);
}

/// One column of margin on each side, unless the terminal is very narrow.
fn inset(r: Rect) -> Rect {
    if r.width < 30 {
        return r;
    }
    Rect {
        x: r.x + 1,
        width: r.width - 2,
        ..r
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let th = t();
    let [b, d, w, i] = app.counts();
    let mut left = vec![
        Span::raw(" "),
        pill("koolie", th.brand, th.cream),
        Span::raw("  "),
    ];
    for (n, s) in [
        (b, AgentStatus::Blocked),
        (d, AgentStatus::Done),
        (w, AgentStatus::Working),
        (i, AgentStatus::Idle),
    ] {
        let (glyph, st) = if n > 0 {
            (status_glyph(app, s), status_style(s))
        } else {
            (
                status_glyph_static(s).to_string(),
                Style::new().fg(th.faint),
            )
        };
        left.push(Span::styled(format!("{glyph} {} {n}", status_label(s)), st));
        left.push(Span::raw("  "));
    }
    // Hosts: the ones that need attention, or a quiet all-good count.
    let mut right: Vec<Span> = Vec::new();
    let down: Vec<_> = app
        .hosts
        .iter()
        .filter(|h| !h.state.is_connected())
        .collect();
    if down.is_empty() {
        if !app.hosts.is_empty() {
            right.push(Span::styled("● ", Style::new().fg(th.green)));
            right.push(Span::styled(
                format!("ホスト {}", app.hosts.len()),
                Style::new().fg(th.subtle),
            ));
        }
    } else {
        for (k, h) in down.iter().enumerate() {
            if k > 0 {
                right.push(Span::styled(" • ", Style::new().fg(th.faint)));
            }
            right.push(Span::styled(
                format!("⚠ {} {}", h.cfg.name, h.state.label()),
                Style::new().fg(conn_color(&h.state)),
            ));
        }
    }
    right.push(Span::raw(" "));
    f.render_widget(
        Paragraph::new(line_lr(left, right, area.width as usize)),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let th = t();
    let width = area.width as usize;
    if let Some((msg, _)) = &app.toast {
        let bar = Style::new().fg(th.bar_fg).bg(th.bar_bg);
        let spans = vec![
            Span::raw(" "),
            pill("通知", th.accent, th.ink),
            Span::styled(format!(" {msg}"), bar),
        ];
        let line = widgets::truncate_spans(spans, width);
        let line = if th.has_color() {
            pad_spans(line, width.saturating_sub(1), bar)
        } else {
            line
        };
        f.render_widget(Paragraph::new(Line::from(line)), area);
        return;
    }
    if app.view == View::Detail && app.detail.as_ref().map(|d| d.focus) == Some(Focus::Direct) {
        let spans = vec![
            Span::raw(" "),
            pill("直接操作中", th.green, th.ink),
            Span::styled(
                " キーはすべてエージェントへ送られます（Esc も）",
                Style::new().fg(th.green),
            ),
            Span::styled(" • ", Style::new().fg(th.faint)),
            Span::styled(
                "ctrl+g",
                Style::new().fg(th.subtle).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" 戻る", Style::new().fg(th.muted)),
        ];
        f.render_widget(
            Paragraph::new(Line::from(widgets::truncate_spans(spans, width))),
            area,
        );
        return;
    }
    let main = hints(app);
    // `?` only where it opens the help (not while typing).
    let help: &[(&str, &str)] = &[("?", "ヘルプ")];
    let tail: &[(&str, &str)] = match app.view {
        View::Console if !app.console.searching => &[("?", "ヘルプ"), ("q", "終了")],
        View::Detail if app.detail.as_ref().is_some_and(|d| d.focus == Focus::View) => help,
        View::Hosts if app.hosts_view.add.is_none() => help,
        View::Usage => help,
        _ => &[],
    };
    f.render_widget(Paragraph::new(help_line(&main, tail, width)), area);
}

/// The help line's entries for the current view (`?` and `q` come after).
fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let filtered = app.console.filter != crate::app::Filter::All
        || app.console.host_filter.is_some()
        || !app.console.search.is_empty();
    match app.view {
        View::Console if app.console.searching => {
            vec![("文字入力", "検索"), ("enter", "確定"), ("esc", "取消")]
        }
        View::Console => {
            let mut v = vec![
                ("↑↓/jk", "移動"),
                ("enter", "開く"),
                ("c", "チャット"),
                ("a", "端末へ"),
                ("/", "検索"),
                ("n", "要対応"),
                ("h", "ホスト絞込"),
            ];
            if filtered {
                v.insert(0, ("esc", "絞り込み解除"));
            }
            v.extend([("H", "ホスト"), ("u", "使用量")]);
            v
        }
        View::Detail => match app.detail.as_ref().map(|d| d.focus) {
            // The way out comes early so it survives a narrow terminal.
            Some(Focus::Compose) => vec![
                ("enter", "送信"),
                ("esc", "操作モード"),
                ("alt+enter", "改行"),
                ("tab", "画面/チャット"),
                ("ctrl+t", "スニペット"),
                ("pgup/pgdn", "スクロール"),
            ],
            _ => vec![
                ("i", "入力"),
                ("esc/q", "一覧へ"),
                ("tab", "画面/チャット"),
                ("p", "直接操作"),
                ("a", "端末へ"),
                ("x", "中断"),
                ("j/k", "スクロール"),
            ],
        },
        View::Hosts => match app.hosts_view.add {
            Some(AddStep::Choose { .. }) => vec![
                ("↑↓", "移動"),
                ("space", "選択"),
                ("文字入力", "名前を編集"),
                ("enter", "追加"),
                ("esc", "取消"),
            ],
            Some(AddStep::Manual { .. }) => vec![("enter", "調べる"), ("esc", "取消")],
            Some(AddStep::Probing { .. }) => vec![("esc", "取消")],
            Some(AddStep::Pick { .. }) => {
                vec![("↑↓/jk", "移動"), ("enter", "決定"), ("esc", "取消")]
            }
            None => vec![
                ("↑↓/jk", "移動"),
                ("r", "再接続"),
                ("a", "追加"),
                ("x", "削除"),
                ("esc", "戻る"),
            ],
        },
        View::Usage => vec![("tab/←→", "ホスト切替"), ("r", "更新"), ("esc", "戻る")],
        View::Help => vec![("何かキー", "閉じる")],
    }
}

fn location(a: &Agent) -> String {
    let ws = a.workspace_label.clone().unwrap_or_default();
    let cwd = a.cwd.as_deref().map(short_path).unwrap_or_default();
    match (ws.is_empty(), cwd.is_empty()) {
        (false, false) => format!("{ws} · {cwd}"),
        (false, true) => ws,
        (true, _) => cwd,
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests;
