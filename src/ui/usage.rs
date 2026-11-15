//! Usage: tokens and estimated cost, with host tabs and gradient bars.

use super::text::{str_width, truncate};
use super::theme::t;
use super::widgets::pill;
use crate::app::App;
use crate::usage::UsageRow;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

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

pub(super) fn fmt_tokens(n: u64) -> String {
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

/// One period's cell, 20 columns: tokens, then cost (`+` when a model had
/// no price).
fn tot_spans(t: Tot, tokens: Style, cost: Style) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{:>8}", fmt_tokens(t.tokens)), tokens),
        Span::raw("  "),
        Span::styled(format!("${:>8.2}", t.cost), cost),
        Span::styled(
            if t.unpriced { "+" } else { " " },
            Style::new().fg(super::theme::t().subtle),
        ),
    ]
}

/// Pad to `w` display columns (left-aligned).
fn pad_right(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(str_width(s))))
}

/// Pad to `w` display columns (right-aligned).
fn pad_left(s: &str, w: usize) -> String {
    format!("{}{s}", " ".repeat(w.saturating_sub(str_width(s))))
}

fn heading(title: &str, note: &str) -> Line<'static> {
    let th = t();
    Line::from(vec![
        Span::styled(
            format!(" {title}"),
            Style::new().fg(th.indigo).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {note}"), Style::new().fg(th.subtle)),
    ])
}

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect) {
    let th = t();
    let mut lines: Vec<Line> = Vec::new();

    // Tabs: all hosts, then each host with a report.
    let mut tabs = vec![Span::raw(" ")];
    let names: Vec<Option<&String>> = std::iter::once(None)
        .chain(app.usage.reports.keys().map(Some))
        .collect();
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            tabs.push(Span::raw(" "));
        }
        let label = n.map(|s| s.as_str()).unwrap_or("すべて");
        if app.usage.host.as_ref() == *n {
            tabs.push(pill(label, th.brand, th.cream));
        } else {
            tabs.push(Span::styled(
                format!(" {label} "),
                Style::new().fg(th.subtle),
            ));
        }
    }
    lines.push(Line::from(tabs));
    lines.push(Line::raw(""));

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
            Err(e) => lines.push(Line::styled(
                format!(" {host}: {e}"),
                Style::new().fg(th.muted),
            )),
        }
    }
    for h in &app.usage.loading {
        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", app.spinner()), Style::new().fg(th.accent)),
            Span::styled(format!("{h} を集計しています…"), Style::new().fg(th.muted)),
        ]));
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
    const TABLE_W: usize = 1 + 12 + 1 + 20 + 2 + 20 + 2 + 20;
    // Today stands out (fuchsia); the longer periods stay quiet.
    let today_tok = Style::new().fg(th.accent_dim);
    let today_cost = Style::new().fg(th.accent).add_modifier(Modifier::BOLD);
    let tok = Style::new().fg(th.muted);
    let cost = Style::new();
    let head = Style::new().fg(th.subtle);
    lines.push(Line::from(vec![
        Span::raw(format!(" {} {}", pad_right("", 12), " ".repeat(14))),
        pill("今日", th.accent, th.ink),
        Span::styled(
            format!("  {}  {}", pad_left("7日", 20), pad_left("30日", 20)),
            head,
        ),
    ]));
    let period_row = |label: Span<'static>, tot: &[Tot; 3], bold: bool| {
        let b = |s: Style| {
            if bold {
                s.add_modifier(Modifier::BOLD)
            } else {
                s
            }
        };
        let mut v = vec![label, Span::raw(" ")];
        v.extend(tot_spans(tot[0], b(today_tok), today_cost));
        v.push(Span::raw("  "));
        v.extend(tot_spans(tot[1], b(tok), b(cost)));
        v.push(Span::raw("  "));
        v.extend(tot_spans(tot[2], b(tok), b(cost)));
        Line::from(v)
    };
    for (p, tot) in &by_prog {
        lines.push(period_row(
            Span::styled(format!(" {}", pad_right(p, 12)), Style::new().fg(th.indigo)),
            tot,
            false,
        ));
    }
    lines.push(Line::styled(
        format!(
            " {}",
            "─".repeat(TABLE_W.min(area.width as usize).saturating_sub(1))
        ),
        Style::new().fg(th.faint),
    ));
    lines.push(period_row(
        Span::styled(
            format!(" {}", pad_right("合計", 12)),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        &all,
        true,
    ));
    lines.push(Line::raw(""));

    // What is left goes to the models and the daily chart.
    let mut models: Vec<_> = by_model.into_iter().collect();
    models.sort_by(|a, b| {
        b.1.cost
            .total_cmp(&a.1.cost)
            .then(b.1.tokens.cmp(&a.1.tokens))
    });
    // Model heading + blank, chart heading, axis, dates, stats, blank, note.
    let fixed = lines.len() + 2 + 1 + 3 + 2;
    let room = (area.height as usize).saturating_sub(fixed);
    let n_models = models.len().min(8).min(room.saturating_sub(3));
    let chart_h = room.saturating_sub(n_models).clamp(3, 10);

    lines.push(heading("モデル別", "30日"));
    for ((p, m), tot) in models.iter().take(n_models) {
        let mut v = vec![
            Span::styled(format!(" {}", pad_right(p, 8)), Style::new().fg(th.indigo)),
            Span::raw(format!(" {} ", pad_right(&truncate(m, 32), 32))),
        ];
        v.extend(tot_spans(*tot, tok, cost));
        lines.push(Line::from(v));
    }
    lines.push(Line::raw(""));

    lines.push(heading("日別の概算コスト", "30日・API の定価"));
    let days: Vec<(String, f64)> = (0..30)
        .rev()
        .map(|n| {
            let d = day(n);
            let c = by_day.get(d.as_str()).map(|t| t.cost).unwrap_or(0.0);
            (d, c)
        })
        .collect();
    if days.iter().all(|(_, c)| *c <= 0.0) {
        lines.push(Line::styled(
            "   まだ記録がありません",
            Style::new().fg(th.muted),
        ));
    } else {
        lines.extend(daily_chart(&days, area.width as usize, chart_h));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        " ※ API の定価での概算です。サブスクリプションの請求額ではありません。+ は価格不明のモデルを含みます",
        Style::new().fg(th.subtle),
    ));
    f.render_widget(Paragraph::new(lines), area);
}

/// `MM/DD` of a `YYYY-MM-DD` date.
fn short_date(d: &str) -> String {
    d.get(5..).unwrap_or(d).replace('-', "/")
}

/// Daily cost as columns, oldest on the left and today on the right, `h`
/// rows tall. Each row is one step of Charm's violet→fuchsia gradient, so
/// taller columns reach further into pink. Eighth blocks give the tops.
/// Below: the axis, dates, and a line with the total, peak and today.
pub(super) fn daily_chart(days: &[(String, f64)], width: usize, h: usize) -> Vec<Line<'static>> {
    const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let th = t();
    if days.is_empty() {
        return Vec::new();
    }
    let h = h.max(1);
    let max = days.iter().map(|(_, c)| *c).fold(0.0, f64::max);
    let top_label = format!("${max:.2}");
    let label_w = str_width(&top_label).max(2);
    let prefix = label_w + 2; // label, space, axis
    let inner = width.saturating_sub(prefix + 1);
    // Wide columns while 30 days fit; otherwise the most recent days that do.
    let (bar, gap, n) = [(2usize, 1usize), (1, 1), (1, 0)]
        .iter()
        .find(|(b, g)| days.len() * (b + g) - g <= inner)
        .map(|(b, g)| (*b, *g, days.len()))
        .unwrap_or((1, 0, inner.min(days.len())));
    let shown = &days[days.len() - n..];
    let eighths: Vec<usize> = shown
        .iter()
        .map(|(_, c)| {
            if *c > 0.0 && max > 0.0 {
                ((c / max) * (h * 8) as f64).round().max(1.0) as usize
            } else {
                0
            }
        })
        .collect();
    let axis = Style::new().fg(th.faint);
    let label = Style::new().fg(th.subtle);
    let (from, to) = super::theme::GRADIENT;
    let mut out = Vec::new();
    for r in (0..h).rev() {
        let pos = if h > 1 {
            r as f64 / (h - 1) as f64
        } else {
            1.0
        };
        let mut cols = String::new();
        for (i, e) in eighths.iter().enumerate() {
            if i > 0 {
                cols.push_str(&" ".repeat(gap));
            }
            let fill = e.saturating_sub(r * 8).min(8);
            cols.push_str(&EIGHTHS[fill].to_string().repeat(bar));
        }
        let y = if r == h - 1 {
            top_label.clone()
        } else if r == 0 {
            "$0".to_string()
        } else {
            String::new()
        };
        out.push(Line::from(vec![
            Span::styled(format!("{} ", pad_left(&y, label_w)), label),
            Span::styled("│", axis),
            Span::styled(cols, Style::new().fg(th.rgb(from.mix(to, pos)))),
        ]));
    }
    let chart_w = if n == 0 { 0 } else { n * (bar + gap) - gap };
    out.push(Line::from(vec![
        Span::raw(" ".repeat(label_w + 1)),
        Span::styled(format!("└{}", "─".repeat(chart_w)), axis),
    ]));
    // Dates: the first day, the middle one, and today under its column.
    let mut marks: Vec<(usize, String, Style)> = Vec::new();
    if let Some((d, _)) = shown.first() {
        marks.push((0, short_date(d), label));
    }
    if n >= 3 {
        let mid = n / 2;
        marks.push((mid * (bar + gap), short_date(&shown[mid].0), label));
    }
    let today = "今日";
    marks.push((
        chart_w.saturating_sub(str_width(today)),
        today.to_string(),
        Style::new().fg(th.accent).add_modifier(Modifier::BOLD),
    ));
    let mut v = vec![Span::raw(" ".repeat(prefix))];
    let mut pos = 0;
    for (at, text, style) in marks {
        let w = str_width(&text);
        // Skip a mark that would touch the one before it or run past the end.
        let min_at = if pos == 0 { 0 } else { pos + 1 };
        if at < min_at || at + w > chart_w.max(w) {
            continue;
        }
        v.push(Span::raw(" ".repeat(at - pos)));
        v.push(Span::styled(text, style));
        pos = at + w;
    }
    out.push(Line::from(v));
    // Total, the peak day, today.
    let total: f64 = shown.iter().map(|(_, c)| c).sum();
    let peak = shown
        .iter()
        .filter(|(_, c)| *c > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let today_cost = shown.last().map(|(_, c)| *c).unwrap_or(0.0);
    let sep = Span::styled("  •  ", Style::new().fg(th.faint));
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut stats = vec![
        Span::raw(" ".repeat(prefix)),
        Span::styled("合計 ", label),
        Span::styled(format!("${total:.2}"), bold),
    ];
    if let Some((d, c)) = peak {
        stats.push(sep.clone());
        stats.push(Span::styled("最大 ", label));
        stats.push(Span::styled(format!("${c:.2}"), bold));
        stats.push(Span::styled(format!(" ({})", short_date(d)), label));
    }
    stats.push(sep);
    stats.push(Span::styled("今日 ", label));
    stats.push(Span::styled(
        format!("${today_cost:.2}"),
        Style::new().fg(th.accent).add_modifier(Modifier::BOLD),
    ));
    out.push(Line::from(super::widgets::truncate_spans(stats, width)));
    out
}
