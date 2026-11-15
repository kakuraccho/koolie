//! Hosts: a borderless table (bubbles table) and the add-host form, drawn
//! like a huh form: a thick bar on the left, a violet title, `>` on the
//! current choice.

use super::text::{str_width, truncate, wrap};
use super::theme::t;
use super::widgets::{input_spans, pill, truncate_spans};
use super::{centered, conn_color};
use crate::app::{AddStep, App};
use crate::discover::CandidateSource;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Table, TableState,
};

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect) {
    let th = t();
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
                Cell::from(Span::styled(
                    h.cfg.name.clone(),
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(
                    h.cfg.kind.as_str(),
                    Style::new().fg(th.indigo),
                )),
                Cell::from(Span::styled(place, Style::new().fg(th.muted))),
                Cell::from(Line::from(vec![
                    Span::styled("● ", Style::new().fg(conn_color(&h.state))),
                    Span::raw(h.state.label()),
                ])),
                Cell::from(Span::styled(
                    h.version.clone().unwrap_or_default(),
                    Style::new().fg(th.muted),
                )),
                Cell::from(agents),
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
            Constraint::Length(4),
        ],
    )
    .header(
        Row::new(["名前", "種類", "接続先", "状態", "版", "数"])
            .style(Style::new().fg(th.subtle))
            .bottom_margin(1),
    )
    .column_spacing(2)
    .row_highlight_style(th.selected_row())
    .highlight_symbol(" ");
    let mut st = TableState::default().with_selected(Some(app.hosts_view.selected));
    f.render_stateful_widget(table, list, &mut st);
    // The rule under the header goes in the header's bottom margin.
    if list.height > 2 {
        let rule = Rect {
            y: list.y + 1,
            height: 1,
            ..list
        };
        f.render_widget(
            Paragraph::new(Line::styled(
                "─".repeat(list.width as usize),
                Style::new().fg(th.faint),
            )),
            rule,
        );
    }

    draw_host_info(f, app, detail);

    if let Some(step) = &app.hosts_view.add {
        draw_add(f, app, step, area);
    }
}

fn draw_host_info(f: &mut Frame, app: &App, area: Rect) {
    let th = t();
    let width = area.width as usize;
    let label = Style::new().fg(th.subtle);
    let sep = Span::styled("  •  ", Style::new().fg(th.faint));
    let mut v = vec![Line::styled("─".repeat(width), Style::new().fg(th.faint))];
    match app.hosts.get(app.hosts_view.selected) {
        Some(h) => {
            let route = if h.cfg.is_local() {
                "ローカル".into()
            } else {
                h.cfg.routes.join(" → ")
            };
            v.push(Line::from(truncate_spans(
                vec![
                    Span::raw(" "),
                    Span::styled(
                        h.cfg.name.clone(),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("   "),
                    Span::styled("セッション ", label),
                    Span::raw(h.cfg.session_name().to_string()),
                    sep,
                    Span::styled("経路 ", label),
                    Span::raw(route),
                ],
                width,
            )));
            if let Some(e) = h.state.error() {
                for l in wrap(e, width.saturating_sub(2)).into_iter().take(2) {
                    v.push(Line::styled(format!(" {l}"), Style::new().fg(th.pink)));
                }
            }
            if app.hosts_view.confirm_remove {
                v.push(Line::from(truncate_spans(
                    vec![
                        Span::raw(" "),
                        pill("削除", th.pink, th.ink),
                        Span::styled(
                            format!(" {} を設定から削除しますか？ y で削除", h.cfg.name),
                            Style::new().fg(th.pink).add_modifier(Modifier::BOLD),
                        ),
                    ],
                    width,
                )));
            }
        }
        None => v.push(Line::styled(
            " ホストがありません。a で追加できます",
            Style::new().fg(th.muted),
        )),
    }
    f.render_widget(Paragraph::new(v), area);
}

/// Lines of a huh-style field: everything behind a thick left bar.
struct Form {
    lines: Vec<Line<'static>>,
    width: usize,
}

impl Form {
    fn new(width: usize) -> Form {
        Form {
            lines: Vec::new(),
            width,
        }
    }

    fn push(&mut self, spans: Vec<Span<'static>>) {
        let th = t();
        let mut v = vec![Span::styled("┃ ", Style::new().fg(th.faint))];
        v.extend(spans);
        self.lines.push(Line::from(truncate_spans(v, self.width)));
    }

    fn title(&mut self, s: &str) {
        let th = t();
        self.push(vec![Span::styled(
            s.to_string(),
            Style::new().fg(th.indigo).add_modifier(Modifier::BOLD),
        )]);
    }

    fn note(&mut self, s: &str) {
        let th = t();
        for l in wrap(s, self.width.saturating_sub(2)) {
            self.push(vec![Span::styled(l, Style::new().fg(th.muted))]);
        }
    }

    fn blank(&mut self) {
        self.push(Vec::new());
    }

    /// A choice: `> ` on the current one.
    fn option(&mut self, current: bool, spans: Vec<Span<'static>>) {
        let th = t();
        let mut v = vec![Span::styled(
            if current { "> " } else { "  " },
            Style::new().fg(th.accent),
        )];
        v.extend(spans);
        self.push(v);
    }
}

fn draw_add(f: &mut Frame, app: &App, step: &AddStep, area: Rect) {
    let th = t();
    let w = (area.width.saturating_sub(4)).min(90);
    let max_h = area.height.saturating_sub(2).min(20);
    // Borders take 2 rows, padding 1 above and 1 below.
    let room = max_h.saturating_sub(4) as usize;
    let mut form = Form::new(w.saturating_sub(4) as usize);
    let on = Style::new().fg(th.green);
    let muted = Style::new().fg(th.muted);
    let spinner = Style::new().fg(th.accent);
    match step {
        AddStep::Pick { selected, loading } => {
            form.title("接続先");
            form.note("SSH で接続します。候補から選ぶか、手で入力してください");
            form.blank();
            let cur = *selected == 0;
            form.option(
                cur,
                vec![
                    Span::styled("✎ 手入力", if cur { on } else { Style::new() }),
                    Span::styled("  user@host、ssh config の別名、tailnet の名前", muted),
                ],
            );
            if *loading {
                form.push(vec![
                    Span::styled(format!("  {} ", app.spinner()), spinner),
                    Span::styled("候補を探しています…", muted),
                ]);
            }
            let visible = room.saturating_sub(form.lines.len());
            let skip = selected.saturating_sub(visible);
            let name_w = app
                .hosts_view
                .candidates
                .iter()
                .map(|c| str_width(&c.name))
                .max()
                .unwrap_or(0)
                .min(20);
            let target_w = app
                .hosts_view
                .candidates
                .iter()
                .map(|c| str_width(&c.target))
                .max()
                .unwrap_or(0)
                .min(36);
            for (i, c) in app
                .hosts_view
                .candidates
                .iter()
                .enumerate()
                .skip(skip)
                .take(visible)
            {
                let cur = *selected == i + 1;
                let offline = c.online == Some(false);
                let src = match c.source {
                    CandidateSource::SshConfig => "ssh config",
                    CandidateSource::Tailnet => "tailnet",
                    CandidateSource::HerdrMachine => "herdr",
                };
                let name_style = if cur {
                    on.add_modifier(Modifier::BOLD)
                } else if offline {
                    muted
                } else {
                    Style::new()
                };
                let mut spans = vec![
                    Span::styled(format!("{src:<10} "), Style::new().fg(th.subtle)),
                    Span::styled(
                        format!(
                            "{}{}",
                            c.name,
                            " ".repeat(name_w.saturating_sub(str_width(&c.name)))
                        ),
                        name_style,
                    ),
                    Span::styled(
                        format!(
                            "  {}{}",
                            c.target,
                            " ".repeat(target_w.saturating_sub(str_width(&c.target)))
                        ),
                        muted,
                    ),
                ];
                if let Some(os) = &c.os {
                    spans.push(Span::styled(format!("  {os}"), Style::new().fg(th.subtle)));
                }
                match c.online {
                    Some(true) => spans.push(Span::styled("  ●", on)),
                    Some(false) => spans.push(Span::styled("  ○ offline", muted)),
                    None => {}
                }
                form.option(cur, spans);
            }
        }
        AddStep::Manual { input } => {
            form.title("SSH の接続先");
            form.note("user@host、ssh config の別名、tailnet の名前");
            form.blank();
            let mut v = vec![Span::styled("> ", Style::new().fg(th.accent))];
            v.extend(input_spans(
                &input.before_cursor(),
                &input.after_cursor(),
                Style::new(),
            ));
            form.push(v);
            form.blank();
            form.note(
                "鍵認証でパスワードなしに入れる必要があります（koolie はパスワードを聞きません）",
            );
        }
        AddStep::Probing { target } => {
            form.title("調べています");
            form.blank();
            form.push(vec![
                Span::styled(format!("{} ", app.spinner()), spinner),
                Span::raw(format!("{target} を調べています…")),
            ]);
        }
        AddStep::Choose {
            target,
            name,
            options,
            selected,
        } => {
            form.title(&format!("{target} で見つかったサーバー"));
            form.note("登録するものを space で選んでください");
            form.blank();
            let mut v = vec![
                Span::styled("名前 ", Style::new().fg(th.subtle)),
                Span::styled("> ", Style::new().fg(th.accent)),
            ];
            v.extend(input_spans(
                &name.before_cursor(),
                &name.after_cursor(),
                Style::new(),
            ));
            form.push(v);
            form.blank();
            for (i, (label, _, picked)) in options.iter().enumerate() {
                let (mark, style) = if *picked {
                    ("✓ ", on)
                } else {
                    ("• ", Style::new().fg(th.subtle))
                };
                form.option(
                    i == *selected,
                    vec![
                        Span::styled(mark, style),
                        Span::styled(
                            truncate(label, form.width),
                            if *picked { on } else { Style::new() },
                        ),
                    ],
                );
            }
        }
    }
    let h = (form.lines.len().min(room) as u16 + 4).min(max_h);
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th.brand))
        .padding(Padding::new(1, 1, 1, 1))
        .title(Span::styled(
            " ホストを追加 ",
            Style::new().fg(th.brand).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(form.lines).block(block), r);
}
