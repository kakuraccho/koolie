//! Drawing. Pure functions of [`App`] state.

pub mod text;

use crate::app::{AddStep, App, DetailMode, DetailState, Focus, View};
use crate::model::{Agent, AgentStatus, short_path};
use crate::session::ConnState;
use crate::transcript::{Conversation, Step};
use crate::usage::UsageRow;
use ansi_to_tui::IntoText;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Row, Table, TableState};
use text::{str_width, truncate, wrap, wrap_prefixed};

const DIM: Color = Color::DarkGray;

pub fn status_style(s: AgentStatus) -> Style {
    match s {
        AgentStatus::Blocked => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        AgentStatus::Done => Style::new().fg(Color::Green),
        AgentStatus::Working => Style::new().fg(Color::Yellow),
        AgentStatus::Idle => Style::new().fg(Color::Gray),
        AgentStatus::Unknown => Style::new().fg(DIM),
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
        AgentStatus::Blocked => "●".into(),
        AgentStatus::Done => "✔".into(),
        AgentStatus::Working => app.spinner().to_string(),
        AgentStatus::Idle => "○".into(),
        AgentStatus::Unknown => "?".into(),
    }
}

fn conn_style(s: &ConnState) -> Style {
    match s {
        ConnState::Connected => Style::new().fg(Color::Green),
        ConnState::Connecting | ConnState::Reconnecting { .. } => Style::new().fg(Color::Yellow),
        ConnState::Failed { .. } => Style::new().fg(Color::Red),
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_header(f, app, header);
    match app.view {
        View::Console => draw_console(f, app, body),
        View::Detail => draw_detail(f, app, body),
        View::Hosts => draw_hosts(f, app, body),
        View::Usage => draw_usage(f, app, body),
        View::Help => {
            match app.prev_view {
                View::Detail => draw_detail(f, app, body),
                View::Hosts => draw_hosts(f, app, body),
                View::Usage => draw_usage(f, app, body),
                _ => draw_console(f, app, body),
            }
            draw_help(f, app, body);
        }
    }
    draw_footer(f, app, footer);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let [b, d, w, i] = app.counts();
    let mut spans = vec![
        Span::styled(
            " koolie ",
            Style::new().fg(Color::Black).bg(Color::Cyan).bold(),
        ),
        Span::raw("  "),
    ];
    let mut count = |n: usize, s: AgentStatus| {
        let st = if n > 0 {
            status_style(s)
        } else {
            Style::new().fg(DIM)
        };
        spans.push(Span::styled(
            format!("{} {} {n}", status_glyph_static(s), status_label(s)),
            st,
        ));
        spans.push(Span::raw("  "));
    };
    count(b, AgentStatus::Blocked);
    count(d, AgentStatus::Done);
    count(w, AgentStatus::Working);
    count(i, AgentStatus::Idle);
    // Hosts that are not connected.
    for h in &app.hosts {
        if !h.state.is_connected() {
            spans.push(Span::styled(
                format!("⚠ {} {}  ", h.cfg.name, h.state.label()),
                conn_style(&h.state),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, _)) = &app.toast {
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                format!(" {msg}"),
                Style::new().fg(Color::Black).bg(Color::Yellow),
            )])),
            area,
        );
        return;
    }
    let hints = match app.view {
        View::Console if app.console.searching => "入力して検索  Enter 確定  Esc 取消",
        View::Console => {
            "j/k 移動  Enter 開く  c チャット  a 端末へ  / 検索  n 要対応  h ホスト絞込  H ホスト  u 使用量  ? ヘルプ  q 終了"
        }
        View::Detail => match app.detail.as_ref().map(|d| d.focus) {
            Some(Focus::Compose) => {
                "Enter 送信  Alt+Enter 改行  Tab 画面/チャット  PgUp/PgDn スクロール  Ctrl+T スニペット  Esc 操作モード"
            }
            _ => "i 入力  Tab 画面/チャット  a 端末へ  x 中断  j/k スクロール  Esc/q 一覧へ",
        },
        View::Hosts => match app.hosts_view.add {
            Some(AddStep::Choose { .. }) => {
                "Space 選択  文字入力で名前を編集  Enter 追加  Esc 取消"
            }
            Some(_) => "j/k 移動  Enter 決定  Esc 取消",
            None => "j/k 移動  r 再接続  a 追加  x 削除  Esc 戻る",
        },
        View::Usage => "Tab/←→ ホスト切替  r 更新  Esc 戻る",
        View::Help => "何かキーを押すと閉じます",
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {hints}"),
            Style::new().fg(DIM),
        ))),
        area,
    );
}

// ------------------------------------------------------------------ console

fn draw_console(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.rows();
    let mut title = format!(" エージェント {} ", rows.len());
    if app.console.filter != crate::app::Filter::All {
        title.push_str(&format!("[{}] ", app.console.filter.label()));
    }
    if let Some(h) = &app.console.host_filter {
        title.push_str(&format!("[ホスト: {h}] "));
    }
    if app.console.searching || !app.console.search.is_empty() {
        title.push_str(&format!(
            "[検索: {}{}] ",
            app.console.search,
            if app.console.searching { "▏" } else { "" }
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title);
    if rows.is_empty() {
        let msg = empty_console_message(app);
        f.render_widget(
            Paragraph::new(Text::from(
                msg.into_iter().map(Line::from).collect::<Vec<_>>(),
            ))
            .style(Style::new().fg(DIM))
            .block(block),
            area,
        );
        return;
    }
    let multi_host = app.hosts.len() > 1;
    let inner_w = area.width.saturating_sub(2) as usize;
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|a| {
            let live = app.is_live(&a.key);
            let st = status_style(a.status);
            let mut cells = vec![
                Span::styled(
                    format!("{} {}", status_glyph(app, a.status), status_label(a.status)),
                    st,
                ),
                Span::styled(a.program().to_string(), Style::new().fg(Color::Cyan)),
                Span::raw(a.display_title()),
            ];
            if multi_host {
                cells.push(Span::styled(
                    a.key.host.clone(),
                    Style::new().fg(Color::Magenta),
                ));
            }
            cells.push(Span::styled(location(a), Style::new().fg(DIM)));
            let row = Row::new(cells);
            if live {
                row
            } else {
                row.style(Style::new().fg(DIM))
            }
        })
        .collect();
    let mut widths = vec![
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Fill(3),
    ];
    if multi_host {
        widths.push(Constraint::Length(12));
    }
    widths.push(Constraint::Fill(2));
    let _ = inner_w;
    let table = Table::new(table_rows, widths)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(
            Style::new()
                .bg(Color::Rgb(40, 44, 60))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut state = TableState::default().with_selected(Some(app.console.selected));
    f.render_stateful_widget(table, area, &mut state);
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

fn empty_console_message(app: &App) -> Vec<String> {
    if app.hosts.is_empty() {
        return vec![
            String::new(),
            "  herdr が見つかりません。".into(),
            "  このマシンで herdr を起動するか、H → a で別のマシンを追加してください。".into(),
        ];
    }
    let loading = app
        .hosts
        .iter()
        .any(|h| h.inventory.is_none() && !matches!(h.state, ConnState::Failed { .. }));
    if loading {
        return vec![
            String::new(),
            format!("  {} エージェントを読み込んでいます…", app.spinner()),
        ];
    }
    if !app.console.search.is_empty()
        || app.console.filter != crate::app::Filter::All
        || app.console.host_filter.is_some()
    {
        return vec![
            String::new(),
            "  条件に合うエージェントはいません (Esc で絞り込みを解除)".into(),
        ];
    }
    let failed: Vec<String> = app
        .hosts
        .iter()
        .filter_map(|h| h.state.error().map(|e| format!("  {}: {}", h.cfg.name, e)))
        .collect();
    let mut v = vec![String::new(), "  動いているエージェントはいません。".into()];
    if !failed.is_empty() {
        v.push(String::new());
        v.extend(failed);
    }
    v
}

// ------------------------------------------------------------------- detail

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(d) = app.detail.as_ref() else { return };
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

    // Info.
    let live = app.is_live(&d.key);
    let mut l1 = vec![Span::styled(
        format!(" {} {} ", status_glyph(app, status), status_label(status)),
        status_style(status),
    )];
    if let Some(a) = agent {
        l1.push(Span::styled(
            format!("{}  ", a.program()),
            Style::new().fg(Color::Cyan).bold(),
        ));
        l1.push(Span::raw(a.display_title()));
    } else {
        l1.push(Span::styled(
            "このエージェントは見つかりません（終了した可能性があります）",
            Style::new().fg(DIM),
        ));
    }
    if !live {
        l1.push(Span::styled("  [未接続]", Style::new().fg(Color::Red)));
    }
    let mut l2 = vec![Span::styled(
        format!(" {}", d.key.host),
        Style::new().fg(Color::Magenta),
    )];
    if let Some(a) = agent {
        l2.push(Span::styled(
            format!("  {}  {}", d.key.id, location(a)),
            Style::new().fg(DIM),
        ));
    }
    let tabs = match d.mode {
        DetailMode::Peek => "  [画面] チャット",
        DetailMode::Chat => "  画面 [チャット]",
    };
    l2.push(Span::styled(tabs, Style::new().fg(Color::Cyan)));
    f.render_widget(Paragraph::new(vec![Line::from(l1), Line::from(l2)]), info);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if d.focus == Focus::View {
            Color::Cyan
        } else {
            DIM
        }));
    match d.mode {
        DetailMode::Peek => draw_peek(f, d, block, content),
        DetailMode::Chat => draw_chat(f, d, block, content),
    }

    if hint_h > 0 {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" 入力待ち ", Style::new().fg(Color::White).bg(Color::Red).bold()),
                Span::styled(
                    if d.mode == DetailMode::Peek {
                        "  画面の質問に 1-9 / y / n / ↑↓ / Enter で直接答えられます（入力欄にいるときは Esc で抜けてから）"
                    } else {
                        "  Tab で画面に切り替えると、質問にキーで直接答えられます"
                    },
                    Style::new().fg(Color::Red),
                ),
            ])),
            hint,
        );
    }

    // Composer.
    let send_label = if d.busy {
        "送信中…".to_string()
    } else if d.composer.is_blank() && status == AgentStatus::Working {
        "中断は Esc → x".to_string()
    } else if status == AgentStatus::Blocked {
        "Enter: 入力欄に入れる（確定しない）".to_string()
    } else {
        "Enter: 送信".to_string()
    };
    let focused = d.focus == Focus::Compose;
    let cblock = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { Color::Cyan } else { DIM }))
        .title(" メッセージ ")
        .title_bottom(
            Line::from(Span::styled(
                format!(" {send_label} "),
                Style::new().fg(DIM),
            ))
            .right_aligned(),
        );
    let inner = cblock.inner(composer);
    let (lines, (row, col)) = d.composer.layout(inner.width as usize);
    let top = row.saturating_sub(inner.height.saturating_sub(1) as usize);
    let body: Vec<Line> = if d.composer.text().is_empty() && !focused {
        vec![Line::from(Span::styled("i で入力", Style::new().fg(DIM)))]
    } else {
        lines.into_iter().skip(top).map(Line::from).collect()
    };
    f.render_widget(Paragraph::new(body).block(cblock), composer);
    if focused && d.snippet_picker.is_none() {
        f.set_cursor_position((inner.x + col as u16, inner.y + (row - top) as u16));
    }

    if let Some(sel) = d.snippet_picker {
        draw_snippets(f, app, sel, area);
    }
}

fn draw_peek(f: &mut Frame, d: &DetailState, block: Block, area: Rect) {
    let inner_h = block.inner(area).height as usize;
    let text: Text = match (&d.screen, &d.screen_error) {
        (_, Some(e)) if d.screen.is_none() => Text::from(Span::styled(
            format!("読めませんでした: {e}"),
            Style::new().fg(Color::Red),
        )),
        (Some(s), _) => s
            .text
            .replace("\r\n", "\n")
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(s.text.clone())),
        (None, _) => Text::from(Span::styled("読み込み中…", Style::new().fg(DIM))),
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
    let title = if scroll > 0 {
        format!(" 画面 (↑{scroll}) ")
    } else {
        " 画面 ".to_string()
    };
    f.render_widget(Paragraph::new(shown).block(block.title(title)), area);
}

/// Conversation as wrapped, styled lines.
pub fn chat_lines(c: &Conversation, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let you = Style::new().fg(Color::Cyan).bold();
    let tool = Style::new().fg(DIM);
    for (ti, t) in c.turns.iter().enumerate() {
        if ti > 0 {
            out.push(Line::raw(""));
        }
        if let Some(p) = &t.prompt {
            for l in wrap_prefixed(p, width, "❯ ", "  ") {
                out.push(Line::styled(l, you));
            }
        }
        let tools: Vec<&Step> = t
            .steps
            .iter()
            .filter(|s| matches!(s, Step::Tool { .. }))
            .collect();
        if !t.in_progress && t.answer.is_some() {
            if !tools.is_empty() {
                out.push(Line::styled(format!("  ⋯ ツール {} 回", tools.len()), tool));
            }
        } else {
            // Running (or no answer): show the steps.
            for s in &t.steps {
                match s {
                    Step::Tool { name, summary } => {
                        let l = truncate(&format!("  ⚙ {name}: {summary}"), width);
                        out.push(Line::styled(l, tool));
                    }
                    Step::Text(txt) => {
                        for l in wrap_prefixed(txt, width, "  ", "  ") {
                            out.push(Line::styled(l, Style::new().fg(Color::Gray)));
                        }
                    }
                    Step::Thinking => out.push(Line::styled("  … 考え中", tool)),
                }
            }
        }
        if let Some(a) = &t.answer {
            for l in wrap(a, width.saturating_sub(2)) {
                out.push(Line::raw(format!("  {l}")));
            }
        }
        if t.in_progress {
            out.push(Line::styled("  ◐ 作業中…", Style::new().fg(Color::Yellow)));
        }
    }
    out
}

fn draw_chat(f: &mut Frame, d: &DetailState, block: Block, area: Rect) {
    let inner = block.inner(area);
    let mut lines: Vec<Line> = match (&d.chat, &d.chat_error) {
        (Some(c), _) if !c.turns.is_empty() => chat_lines(c, inner.width as usize),
        (_, Some(e)) => wrap(
            &format!("チャットを表示できません: {e}"),
            inner.width as usize,
        )
        .into_iter()
        .map(|l| Line::styled(l, Style::new().fg(Color::Red)))
        .collect(),
        (Some(_), None) => vec![Line::styled("まだ会話がありません", Style::new().fg(DIM))],
        (None, None) => vec![Line::styled("読み込み中…", Style::new().fg(DIM))],
    };
    if let Some((text, _, _)) = &d.pending_echo {
        lines.push(Line::raw(""));
        for l in wrap_prefixed(text, inner.width as usize, "❯ ", "  ") {
            lines.push(Line::styled(l, Style::new().fg(Color::Cyan)));
        }
        lines.push(Line::styled("  送信済み・記録待ち", Style::new().fg(DIM)));
    }
    let h = inner.height as usize;
    let total = lines.len();
    let scroll = d.chat_scroll.min(total.saturating_sub(h));
    let start = total.saturating_sub(h + scroll);
    let shown: Vec<Line> = lines.into_iter().skip(start).take(h).collect();
    let title = if scroll > 0 {
        format!(" チャット (↑{scroll}) ")
    } else {
        " チャット ".into()
    };
    f.render_widget(Paragraph::new(shown).block(block.title(title)), area);
}

fn draw_snippets(f: &mut Frame, app: &App, sel: usize, area: Rect) {
    let h = (app.cfg.snippets.len() as u16 + 2).min(area.height);
    let w = 50.min(area.width);
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let lines: Vec<Line> = app
        .cfg
        .snippets
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let l = truncate(
                &format!("{}  {}", s.name, s.text.replace('\n', " ")),
                (w as usize).saturating_sub(2),
            );
            if i == sel {
                Line::styled(l, Style::new().reversed())
            } else {
                Line::raw(l)
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(" スニペット "),
        ),
        r,
    );
}

// -------------------------------------------------------------------- hosts

fn draw_hosts(f: &mut Frame, app: &App, area: Rect) {
    let [list, detail] = Layout::vertical([Constraint::Min(3), Constraint::Length(5)]).areas(area);
    let rows: Vec<Row> = app
        .hosts
        .iter()
        .map(|h| {
            let agents = h
                .inventory
                .as_ref()
                .map(|i| i.agents.len().to_string())
                .unwrap_or_else(|| "-".into());
            let place = if h.cfg.is_local() {
                "このマシン".to_string()
            } else if h.route.is_empty() {
                h.cfg.routes.join(" | ")
            } else {
                h.route.clone()
            };
            Row::new(vec![
                Span::raw(h.cfg.name.clone()),
                Span::styled(h.cfg.kind.as_str(), Style::new().fg(Color::Cyan)),
                Span::raw(place),
                Span::styled(h.state.label(), conn_style(&h.state)),
                Span::raw(h.version.clone().unwrap_or_default()),
                Span::raw(agents),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Fill(2),
            Constraint::Length(10),
            Constraint::Fill(3),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(6),
        ],
    )
    .header(Row::new(["名前", "種類", "接続先", "状態", "版", "数"]).style(Style::new().fg(DIM)))
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .title(" ホスト "),
    )
    .row_highlight_style(Style::new().bg(Color::Rgb(40, 44, 60)).bold())
    .highlight_symbol("▶ ");
    let mut st = TableState::default().with_selected(Some(app.hosts_view.selected));
    f.render_stateful_widget(table, list, &mut st);

    let info: Vec<Line> = match app.hosts.get(app.hosts_view.selected) {
        Some(h) => {
            let mut v = vec![Line::raw(format!(
                " {}  セッション: {}  経路: {}",
                h.cfg.name,
                h.cfg.session_name(),
                if h.cfg.is_local() {
                    "ローカル".into()
                } else {
                    h.cfg.routes.join(" → ")
                }
            ))];
            if let Some(e) = h.state.error() {
                for l in wrap(e, detail.width.saturating_sub(3) as usize)
                    .into_iter()
                    .take(2)
                {
                    v.push(Line::styled(format!(" {l}"), Style::new().fg(Color::Red)));
                }
            }
            if app.hosts_view.confirm_remove {
                v.push(Line::styled(
                    format!(" {} を設定から削除しますか？ y で削除", h.cfg.name),
                    Style::new().fg(Color::Yellow).bold(),
                ));
            }
            v
        }
        None => vec![Line::styled(
            " ホストがありません。a で追加できます",
            Style::new().fg(DIM),
        )],
    };
    f.render_widget(
        Paragraph::new(info).block(Block::bordered().border_type(BorderType::Rounded)),
        detail,
    );

    if let Some(step) = &app.hosts_view.add {
        draw_add(f, app, step, area);
    }
}

fn draw_add(f: &mut Frame, app: &App, step: &AddStep, area: Rect) {
    let w = (area.width.saturating_sub(4)).min(90);
    let h = area.height.saturating_sub(2).min(20);
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" ホストを追加 ");
    let inner_w = w.saturating_sub(2) as usize;
    let lines: Vec<Line> = match step {
        AddStep::Pick { selected, loading } => {
            let mut v = vec![Line::styled(
                " 接続先を選んでください（SSH で接続します）",
                Style::new().fg(DIM),
            )];
            let first = " ✎ 手入力 (user@host, ssh config の別名, tailnet の名前)".to_string();
            v.push(if *selected == 0 {
                Line::styled(first, Style::new().reversed())
            } else {
                Line::raw(first)
            });
            if *loading {
                v.push(Line::styled(
                    format!(" {} 候補を探しています…", app.spinner()),
                    Style::new().fg(DIM),
                ));
            }
            let visible = h.saturating_sub(5) as usize;
            let skip = selected.saturating_sub(visible);
            for (i, c) in app
                .hosts_view
                .candidates
                .iter()
                .enumerate()
                .skip(skip)
                .take(visible)
            {
                let src = match c.source {
                    crate::discover::CandidateSource::SshConfig => "ssh config",
                    crate::discover::CandidateSource::Tailnet => "tailnet",
                    crate::discover::CandidateSource::HerdrMachine => "herdr",
                };
                let online = match c.online {
                    Some(true) => " ●",
                    Some(false) => " ○ offline",
                    None => "",
                };
                let os =
                    c.os.as_deref()
                        .map(|o| format!(" [{o}]"))
                        .unwrap_or_default();
                let label = truncate(
                    &format!(" {:<10} {}  {}{os}{online}", src, c.name, c.target),
                    inner_w,
                );
                let style = if c.online == Some(false) {
                    Style::new().fg(DIM)
                } else {
                    Style::new()
                };
                v.push(if *selected == i + 1 {
                    Line::styled(label, style.reversed())
                } else {
                    Line::styled(label, style)
                });
            }
            v
        }
        AddStep::Manual { input } => vec![
            Line::styled(" SSH の接続先を入力してください", Style::new().fg(DIM)),
            Line::raw(format!(
                " > {}▏{}",
                input.before_cursor(),
                input.after_cursor()
            )),
            Line::raw(""),
            Line::styled(
                " 鍵認証でパスワードなしに入れる必要があります（koolie はパスワードを聞きません）",
                Style::new().fg(DIM),
            ),
        ],
        AddStep::Probing { target } => vec![Line::raw(format!(
            " {} {target} を調べています…",
            app.spinner()
        ))],
        AddStep::Choose {
            target,
            name,
            options,
            selected,
        } => {
            let mut v = vec![
                Line::raw(format!(" {target} で見つかったサーバー")),
                Line::raw(format!(
                    " 名前: {}▏{}",
                    name.before_cursor(),
                    name.after_cursor()
                )),
                Line::raw(""),
            ];
            for (i, (label, _, on)) in options.iter().enumerate() {
                let l = truncate(
                    &format!(" [{}] {label}", if *on { "x" } else { " " }),
                    inner_w,
                );
                v.push(if i == *selected {
                    Line::styled(l, Style::new().reversed())
                } else {
                    Line::raw(l)
                });
            }
            v
        }
    };
    f.render_widget(Paragraph::new(lines).block(block), r);
}

// -------------------------------------------------------------------- usage

#[derive(Default, Clone, Copy)]
struct Tot {
    tokens: u64,
    cost: f64,
    unpriced: bool,
}

fn add_row(t: &mut Tot, r: &UsageRow) {
    t.tokens += r.input_tokens + r.output_tokens + r.cache_read_tokens + r.cache_write_tokens;
    match r.cost_usd {
        Some(c) => t.cost += c,
        None => t.unpriced = true,
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn fmt_tot(t: Tot) -> String {
    format!(
        "{:>8}  ${:>8.2}{}",
        fmt_tokens(t.tokens),
        t.cost,
        if t.unpriced { "+" } else { " " }
    )
}

/// Pad to `w` display columns (left-aligned).
fn pad_right(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(str_width(s))))
}

/// Pad to `w` display columns (right-aligned).
fn pad_left(s: &str, w: usize) -> String {
    format!("{}{s}", " ".repeat(w.saturating_sub(str_width(s))))
}

fn draw_usage(f: &mut Frame, app: &App, area: Rect) {
    let title = match &app.usage.host {
        None => " 使用量 — すべてのホスト ".to_string(),
        Some(h) => format!(" 使用量 — {h} "),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines: Vec<Line> = Vec::new();
    let today = time::OffsetDateTime::now_utc()
        .to_offset(app.utc_offset)
        .date();
    let day = |n: i64| (today - time::Duration::days(n)).to_string();
    let (d0, d6) = (day(0), day(6));

    let mut rows: Vec<&UsageRow> = Vec::new();
    for (host, r) in &app.usage.reports {
        if app.usage.host.as_ref().is_some_and(|h| h != host) {
            continue;
        }
        match r {
            Ok(rep) => rows.extend(rep.rows.iter()),
            Err(e) => lines.push(Line::styled(format!(" {host}: {e}"), Style::new().fg(DIM))),
        }
    }
    for h in &app.usage.loading {
        lines.push(Line::styled(
            format!(" {} {h} を集計しています…", app.spinner()),
            Style::new().fg(DIM),
        ));
    }
    let mut by_prog: std::collections::BTreeMap<&str, [Tot; 3]> = Default::default();
    let mut all = [Tot::default(); 3];
    let mut by_model: std::collections::BTreeMap<(&str, &str), Tot> = Default::default();
    let mut by_day: std::collections::BTreeMap<&str, Tot> = Default::default();
    for r in &rows {
        let p = by_prog.entry(r.program.as_str()).or_default();
        let periods = [r.date == d0, r.date >= d6, true];
        for (i, inside) in periods.iter().enumerate() {
            if *inside {
                add_row(&mut p[i], r);
                add_row(&mut all[i], r);
            }
        }
        add_row(
            by_model
                .entry((r.program.as_str(), r.model.as_str()))
                .or_default(),
            r,
        );
        add_row(by_day.entry(r.date.as_str()).or_default(), r);
    }
    lines.push(Line::styled(
        format!(
            " {} {}  {}  {}",
            pad_right("", 12),
            pad_left("今日", 20),
            pad_left("7日", 20),
            pad_left("30日", 20)
        ),
        Style::new().fg(DIM),
    ));
    for (p, t) in &by_prog {
        lines.push(Line::raw(format!(
            " {} {}  {}  {}",
            pad_right(p, 12),
            fmt_tot(t[0]),
            fmt_tot(t[1]),
            fmt_tot(t[2])
        )));
    }
    lines.push(Line::styled(
        format!(
            " {} {}  {}  {}",
            pad_right("合計", 12),
            fmt_tot(all[0]),
            fmt_tot(all[1]),
            fmt_tot(all[2])
        ),
        Style::new().bold(),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::styled(" モデル別 (30日)", Style::new().fg(DIM)));
    let mut models: Vec<_> = by_model.into_iter().collect();
    models.sort_by(|a, b| {
        b.1.cost
            .total_cmp(&a.1.cost)
            .then(b.1.tokens.cmp(&a.1.tokens))
    });
    for ((p, m), t) in models.iter().take(8) {
        lines.push(Line::raw(format!(
            " {} {} {}",
            pad_right(p, 8),
            pad_right(&truncate(m, 32), 32),
            fmt_tot(*t)
        )));
    }
    lines.push(Line::raw(""));
    // Daily bars (cost).
    let max = by_day.values().map(|t| t.cost).fold(0.0, f64::max);
    let bar_w = inner.width.saturating_sub(24) as f64;
    lines.push(Line::styled(" 日別の概算コスト", Style::new().fg(DIM)));
    for (d, t) in by_day
        .iter()
        .rev()
        .take(inner.height.saturating_sub(lines.len() as u16 + 2) as usize)
    {
        let n = if max > 0.0 {
            (t.cost / max * bar_w).round() as usize
        } else {
            0
        };
        lines.push(Line::from(vec![
            Span::raw(format!(" {d} ${:>7.2} ", t.cost)),
            Span::styled("█".repeat(n), Style::new().fg(Color::Cyan)),
        ]));
    }
    lines.push(Line::styled(
        " ※ API の定価での概算です。サブスクリプションの請求額ではありません。+ は価格不明のモデルを含みます",
        Style::new().fg(DIM),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

// --------------------------------------------------------------------- help

fn draw_help(f: &mut Frame, _app: &App, area: Rect) {
    let text = [
        ("一覧", ""),
        ("j/k ↑↓", "移動"),
        ("Enter / c", "詳細を開く / チャットで開く"),
        ("a", "エージェントの端末に入る（Ctrl+B → q で戻る）"),
        ("/", "検索"),
        ("n b d w i", "絞り込み: 要対応/入力待ち/完了/作業中/待機"),
        ("h", "ホストで絞り込み"),
        ("H / u", "ホスト管理 / 使用量"),
        ("r", "すべて更新"),
        ("詳細", ""),
        ("i", "メッセージを書く（Enter 送信、Alt+Enter 改行）"),
        ("Tab", "画面 ⇄ チャット"),
        ("1-9 y n ↑↓ Enter", "入力待ちのとき、そのキーを直接送る"),
        ("x", "中断（Esc を送る）"),
        ("Ctrl+T / s", "スニペット"),
        ("Esc", "入力欄から操作モードへ / 一覧へ戻る"),
        ("ホスト管理", ""),
        ("a / r / x", "追加 / 再接続 / 削除"),
        ("使用量", ""),
        ("Tab ←→ / r", "ホスト切り替え / 再集計"),
        ("共通", ""),
        ("Esc / q", "前の画面へ（一覧では q で終了）"),
        ("Ctrl+C", "終了"),
    ];
    let w = 72.min(area.width);
    let h = (text.len() as u16 + 2).min(area.height);
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let lines: Vec<Line> = text
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                Line::styled(format!(" {k}"), Style::new().fg(Color::Cyan).bold())
            } else {
                let pad = 20usize.saturating_sub(str_width(k));
                Line::from(vec![
                    Span::styled(format!("   {k}{}", " ".repeat(pad)), Style::new().bold()),
                    Span::raw(*v),
                ])
            }
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(" ヘルプ "),
        ),
        r,
    );
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
mod tests {
    use super::*;
    use crate::transcript::Turn;

    #[test]
    fn chat_lines_fold_finished_turns() {
        let c = Conversation {
            turns: vec![
                Turn {
                    prompt: Some("直して".into()),
                    steps: vec![
                        Step::Tool {
                            name: "Bash".into(),
                            summary: "cargo test".into(),
                        },
                        Step::Text("見ています".into()),
                    ],
                    answer: Some("直しました".into()),
                    in_progress: false,
                    ..Default::default()
                },
                Turn {
                    prompt: Some("次".into()),
                    steps: vec![Step::Tool {
                        name: "Read".into(),
                        summary: "src/main.rs".into(),
                    }],
                    answer: None,
                    in_progress: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let text: Vec<String> = chat_lines(&c, 40)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text[0], "❯ 直して");
        assert!(text.contains(&"  ⋯ ツール 1 回".to_string()));
        assert!(text.contains(&"  直しました".to_string()));
        assert!(text.contains(&"  ⚙ Read: src/main.rs".to_string()));
        assert_eq!(text.last().unwrap(), "  ◐ 作業中…");
    }

    #[test]
    fn token_format() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1500), "1.5k");
        assert_eq!(fmt_tokens(2_500_000), "2.5M");
    }
}
