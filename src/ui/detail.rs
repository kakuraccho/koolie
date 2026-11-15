//! Detail: one agent's screen or chat, with the Composer underneath.

use super::markdown;
use super::text::{truncate, wrap};
use super::theme::t;
use super::widgets::{line_lr, pill, truncate_spans};
use super::{centered, location, status_color, status_glyph, status_label};
use crate::app::{App, DetailMode, DetailState, Focus};
use crate::model::AgentStatus;
use crate::transcript::{Conversation, Step};
use ansi_to_tui::IntoText;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect) {
    let Some(d) = app.detail.as_ref() else { return };
    let th = t();
    let agent = app.agent(&d.key);
    let status = agent.map(|a| a.status).unwrap_or_default();
    let composer_w = area.width.saturating_sub(4) as usize;
    let (c_lines, _) = d.composer.layout(composer_w.max(2));
    let composer_h = (c_lines.len() as u16 + 2).clamp(3, 10);
    let hint_h = if status == AgentStatus::Blocked { 1 } else { 0 };
    let [info, content, hint, composer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(hint_h),
        Constraint::Length(composer_h),
    ])
    .areas(area);

    // Info: status pill, program, title; the tabs on the right.
    let width = info.width as usize;
    let live = app.is_live(&d.key);
    let mut l1 = vec![
        Span::raw(" "),
        pill(
            &format!("{} {}", status_glyph(app, status), status_label(status)),
            status_color(status),
            th.ink,
        ),
        Span::raw("  "),
    ];
    match agent {
        Some(a) => {
            l1.push(Span::styled(
                a.program().to_string(),
                Style::new().fg(th.indigo).add_modifier(Modifier::BOLD),
            ));
            l1.push(Span::raw("  "));
            l1.push(Span::styled(
                a.display_title(),
                Style::new().add_modifier(Modifier::BOLD),
            ));
        }
        None => l1.push(Span::styled(
            "このエージェントは見つかりません（終了した可能性があります）",
            Style::new().fg(th.muted),
        )),
    }
    let tab = |label: &str, on: bool| {
        if on {
            pill(label, th.brand, th.cream)
        } else {
            Span::styled(format!(" {label} "), Style::new().fg(th.subtle))
        }
    };
    let tabs = vec![
        tab("画面", d.mode == DetailMode::Peek),
        Span::raw(" "),
        tab("チャット", d.mode == DetailMode::Chat),
    ];
    let sep = || Span::styled(" · ", Style::new().fg(th.faint));
    let mut l2 = vec![
        Span::raw(" "),
        Span::styled(
            d.key.host.clone(),
            Style::new().fg(th.muted).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(a) = agent {
        l2.push(sep());
        l2.push(Span::styled(d.key.id.clone(), Style::new().fg(th.subtle)));
        let loc = location(a);
        if !loc.is_empty() {
            l2.push(sep());
            l2.push(Span::styled(loc, Style::new().fg(th.muted)));
        }
    }
    if !live {
        l2.push(Span::raw("  "));
        l2.push(pill("未接続", th.pink, th.ink));
    }
    f.render_widget(
        Paragraph::new(vec![
            line_lr(l1, tabs, width),
            Line::from(truncate_spans(l2, width)),
        ]),
        info,
    );

    let (border, title) = match d.focus {
        Focus::View => (
            th.brand,
            Style::new().fg(th.brand).add_modifier(Modifier::BOLD),
        ),
        Focus::Direct => (
            th.green,
            Style::new().fg(th.green).add_modifier(Modifier::BOLD),
        ),
        Focus::Compose => (th.faint, Style::new().fg(th.subtle)),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border));
    let block = if d.focus == Focus::Direct {
        block.title_top(
            Line::from(vec![
                pill("直接操作中 — ctrl+g で戻る", th.green, th.ink),
                Span::raw(" "),
            ])
            .right_aligned(),
        )
    } else {
        block
    };
    match d.mode {
        DetailMode::Peek => draw_peek(f, app, d, block, title, content),
        DetailMode::Chat => draw_chat(f, app, d, block, title, content),
    }

    if hint_h > 0 {
        let text = if d.mode == DetailMode::Peek {
            " 画面の質問に 1-9 / y / n / ↑↓ / Enter で直接答えられます（入力欄にいるときは Esc で抜けてから）"
        } else {
            " Tab で画面に切り替えると、質問にキーで直接答えられます"
        };
        let spans = vec![
            Span::raw(" "),
            pill("入力待ち", th.pink, th.ink),
            Span::styled(text, Style::new().fg(th.pink)),
        ];
        f.render_widget(
            Paragraph::new(Line::from(truncate_spans(spans, hint.width as usize))),
            hint,
        );
    }

    draw_composer(f, app, d, status, composer);

    if let Some(sel) = d.snippet_picker {
        draw_snippets(f, app, sel, area);
    }
}

fn draw_composer(f: &mut Frame, app: &App, d: &DetailState, status: AgentStatus, area: Rect) {
    let th = t();
    let focused = d.focus == Focus::Compose;
    let (label, label_style) = if d.busy {
        (
            format!("{} 送信中…", app.spinner()),
            Style::new().fg(th.accent),
        )
    } else if d.composer.is_blank() && status == AgentStatus::Working {
        ("中断は Esc → x".to_string(), Style::new().fg(th.subtle))
    } else if status == AgentStatus::Blocked {
        (
            "Enter: 入力欄に入れる（確定しない）".to_string(),
            Style::new().fg(th.pink),
        )
    } else {
        ("Enter: 送信".to_string(), Style::new().fg(th.subtle))
    };
    let cblock = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { th.brand } else { th.faint }))
        .title(Span::styled(
            " メッセージ ",
            if focused {
                Style::new().fg(th.brand).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(th.subtle)
            },
        ))
        .title_bottom(Line::from(Span::styled(format!(" {label} "), label_style)).right_aligned());
    let inner = cblock.inner(area);
    let (lines, (row, col)) = d.composer.layout(inner.width as usize);
    let top = row.saturating_sub(inner.height.saturating_sub(1) as usize);
    let placeholder = Style::new().fg(th.subtle);
    let body: Vec<Line> = if d.composer.text().is_empty() {
        let hint = if focused {
            "メッセージを入力…"
        } else {
            "i でメッセージを書く"
        };
        vec![Line::from(Span::styled(
            truncate(hint, inner.width as usize),
            placeholder,
        ))]
    } else {
        lines.into_iter().skip(top).map(Line::from).collect()
    };
    f.render_widget(Paragraph::new(body).block(cblock), area);
    if focused && d.snippet_picker.is_none() {
        f.set_cursor_position((inner.x + col as u16, inner.y + (row - top) as u16));
    }
}

/// The content box's title: the name and how far it is scrolled.
fn content_title(name: &str, scroll: usize, style: Style) -> Line<'static> {
    let mut v = vec![Span::styled(format!(" {name} "), style)];
    if scroll > 0 {
        v.push(Span::styled(
            format!("↑{scroll} "),
            Style::new().fg(t().subtle),
        ));
    }
    Line::from(v)
}

fn draw_peek(f: &mut Frame, app: &App, d: &DetailState, block: Block, title: Style, area: Rect) {
    let th = t();
    let inner_h = block.inner(area).height as usize;
    let text: Text = match (&d.screen, &d.screen_error) {
        (_, Some(e)) if d.screen.is_none() => Text::from(Span::styled(
            format!("読めませんでした: {e}"),
            Style::new().fg(th.pink),
        )),
        (Some(s), _) => s
            .text
            .replace("\r\n", "\n")
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(s.text.clone())),
        (None, _) => Text::from(Span::styled(
            format!("{} 読み込み中…", app.spinner()),
            Style::new().fg(th.muted),
        )),
    };
    // Trim trailing empty lines, then show the bottom (minus scroll).
    let mut lines = text.lines;
    while lines
        .last()
        .is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
    {
        lines.pop();
    }
    let total = lines.len();
    let max_scroll = total.saturating_sub(inner_h);
    let scroll = d.scroll.min(max_scroll);
    let start = total.saturating_sub(inner_h + scroll);
    let shown: Vec<Line> = lines.into_iter().skip(start).take(inner_h).collect();
    f.render_widget(
        Paragraph::new(shown).block(block.title(content_title("画面", scroll, title))),
        area,
    );
}

/// Conversation as wrapped, styled lines: your prompts behind a violet bar,
/// the work folded once a turn is done, answers rendered as light Markdown.
pub fn chat_lines(c: &Conversation, width: usize, spinner: char) -> Vec<Line<'static>> {
    let th = t();
    let mut out: Vec<Line<'static>> = Vec::new();
    let bar = Style::new().fg(th.brand);
    let you = Style::new().add_modifier(Modifier::BOLD);
    let tool = Style::new().fg(th.subtle);
    for (ti, turn) in c.turns.iter().enumerate() {
        if ti > 0 {
            out.push(Line::raw(""));
        }
        if let Some(p) = &turn.prompt {
            out.extend(prompt_lines(p, width, bar, you));
        }
        let tools = turn
            .steps
            .iter()
            .filter(|s| matches!(s, Step::Tool { .. }))
            .count();
        if !turn.in_progress && turn.answer.is_some() {
            if tools > 0 {
                out.push(Line::styled(format!("  ⋯ ツール {tools} 回"), tool));
            }
        } else {
            // Running (or no answer): show the steps.
            for s in &turn.steps {
                match s {
                    Step::Tool { name, summary } => {
                        let spans = vec![
                            Span::styled("  ⚙ ", tool),
                            Span::styled(
                                name.clone(),
                                Style::new().fg(th.muted).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(format!(": {summary}"), tool),
                        ];
                        out.push(Line::from(truncate_spans(spans, width)));
                    }
                    Step::Text(txt) => {
                        for l in wrap(txt, width.saturating_sub(2)) {
                            out.push(Line::styled(format!("  {l}"), Style::new().fg(th.muted)));
                        }
                    }
                    Step::Thinking => out.push(Line::styled(
                        "  … 考え中",
                        tool.add_modifier(Modifier::ITALIC),
                    )),
                }
            }
        }
        if let Some(a) = &turn.answer {
            out.extend(markdown::render(a, width, "  "));
        }
        if turn.in_progress {
            out.push(Line::styled(
                format!("  {spinner} 作業中…"),
                Style::new().fg(th.yellow),
            ));
        }
    }
    out
}

/// A prompt behind a thick bar, wrapped.
fn prompt_lines(text: &str, width: usize, bar: Style, body: Style) -> Vec<Line<'static>> {
    wrap(text, width.saturating_sub(2))
        .into_iter()
        .map(|l| Line::from(vec![Span::styled("┃ ", bar), Span::styled(l, body)]))
        .collect()
}

fn draw_chat(f: &mut Frame, app: &App, d: &DetailState, block: Block, title: Style, area: Rect) {
    let th = t();
    let inner = block.inner(area);
    let width = inner.width as usize;
    let mut lines: Vec<Line> = match (&d.chat, &d.chat_error) {
        (Some(c), _) if !c.turns.is_empty() => chat_lines(c, width, app.spinner()),
        (_, Some(e)) => wrap(&format!("チャットを表示できません: {e}"), width)
            .into_iter()
            .map(|l| Line::styled(l, Style::new().fg(th.pink)))
            .collect(),
        (Some(_), None) => vec![Line::styled(
            "まだ会話がありません",
            Style::new().fg(th.muted),
        )],
        (None, None) => vec![Line::styled(
            format!("{} 読み込み中…", app.spinner()),
            Style::new().fg(th.muted),
        )],
    };
    if let Some((text, _, _)) = &d.pending_echo {
        lines.push(Line::raw(""));
        lines.extend(prompt_lines(
            text,
            width,
            Style::new().fg(th.accent_dim),
            Style::new().fg(th.muted),
        ));
        lines.push(Line::styled(
            format!("  {} 送信済み・記録待ち", app.spinner()),
            Style::new().fg(th.subtle).add_modifier(Modifier::ITALIC),
        ));
    }
    let h = inner.height as usize;
    let total = lines.len();
    let scroll = d.chat_scroll.min(total.saturating_sub(h));
    let start = total.saturating_sub(h + scroll);
    let shown: Vec<Line> = lines.into_iter().skip(start).take(h).collect();
    f.render_widget(
        Paragraph::new(shown).block(block.title(content_title("チャット", scroll, title))),
        area,
    );
}

/// The snippet picker (huh's select): `>` on the current one.
fn draw_snippets(f: &mut Frame, app: &App, sel: usize, area: Rect) {
    let th = t();
    let h = (app.cfg.snippets.len() as u16 + 2).min(area.height);
    let w = 56.min(area.width);
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let inner_w = (w as usize).saturating_sub(2);
    let lines: Vec<Line> = app
        .cfg
        .snippets
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let on = i == sel;
            let spans = vec![
                Span::styled(if on { " > " } else { "   " }, Style::new().fg(th.accent)),
                Span::styled(
                    s.name.clone(),
                    if on {
                        Style::new().fg(th.green).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new()
                    },
                ),
                Span::styled(
                    format!("  {}", s.text.replace('\n', " ")),
                    Style::new().fg(th.muted),
                ),
            ];
            Line::from(truncate_spans(spans, inner_w))
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(th.brand))
                .title(Span::styled(
                    " スニペット ",
                    Style::new().fg(th.brand).add_modifier(Modifier::BOLD),
                )),
        ),
        r,
    );
}
