//! Small building blocks shared by the views: pills, the help bar, padded
//! lines, gradient bars and the like.

use super::text::{char_width, str_width};
use super::theme::t;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// ` text ` on a colored background.
pub fn pill(text: &str, bg: Color, fg: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), t().pill(bg, fg))
}

pub fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| str_width(&s.content)).sum()
}

/// Cut spans to `max` columns, ending with `…` when something was cut.
pub fn truncate_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    if spans_width(&spans) <= max {
        return spans;
    }
    let mut out = Vec::new();
    let mut w = 0;
    let mut last_style = Style::new();
    for s in spans {
        last_style = s.style;
        let mut cut = String::new();
        let mut full = true;
        for c in s.content.chars() {
            let cw = char_width(c);
            if w + cw + 1 > max {
                full = false;
                break;
            }
            cut.push(c);
            w += cw;
        }
        if !cut.is_empty() {
            out.push(Span::styled(cut, s.style));
        }
        if !full {
            break;
        }
    }
    if max > 0 {
        out.push(Span::styled("…", last_style));
    }
    out
}

/// `left` and `right` on one line of `width` columns, `right` flush right.
/// When both do not fit, `right` goes and `left` is cut.
pub fn line_lr(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let (lw, rw) = (spans_width(&left), spans_width(&right));
    if right.is_empty() || lw + 1 + rw > width {
        return Line::from(truncate_spans(left, width));
    }
    let mut v = left;
    v.push(Span::raw(" ".repeat(width - lw - rw)));
    v.extend(right);
    Line::from(v)
}

/// Pad spans with spaces to `width` columns (for lines with a background).
pub fn pad_spans(mut spans: Vec<Span<'static>>, width: usize, style: Style) -> Vec<Span<'static>> {
    let w = spans_width(&spans);
    if w < width {
        spans.push(Span::styled(" ".repeat(width - w), style));
    }
    spans
}

type Entries<'a> = Vec<(&'a str, &'a str)>;

/// The short help line (bubbles help): `key desc • key desc • …`. When it
/// does not fit, entries of `main` go from the end and `…` marks the cut;
/// `tail` (help, quit) stays while it fits at all.
pub fn help_line(main: &[(&str, &str)], tail: &[(&str, &str)], width: usize) -> Line<'static> {
    let entry_w = |e: &(&str, &str)| str_width(e.0) + 1 + str_width(e.1);
    let list_w = |es: &[(&str, &str)]| {
        es.iter().map(entry_w).sum::<usize>() + 3 * es.len().saturating_sub(1)
    };
    let avail = width.saturating_sub(1);
    let all: Vec<(&str, &str)> = main.iter().chain(tail).copied().collect();
    let (head, cut, tail): (Entries, bool, &[(&str, &str)]) = if list_w(&all) <= avail {
        (all, false, &[])
    } else {
        let keep: &[(&str, &str)] = if list_w(tail) + 4 <= avail { tail } else { &[] };
        let from: Vec<(&str, &str)> = if keep.is_empty() {
            all.clone()
        } else {
            main.to_vec()
        };
        // Room for " • …" and the tail after the head.
        let reserve = 4 + if keep.is_empty() { 0 } else { 3 + list_w(keep) };
        let mut head = Vec::new();
        for e in from {
            let w = list_w(&head) + if head.is_empty() { 0 } else { 3 } + entry_w(&e);
            if w + reserve > avail {
                break;
            }
            head.push(e);
        }
        (head, true, keep)
    };
    let th = t();
    let key = Style::new().fg(th.subtle).add_modifier(Modifier::BOLD);
    let desc = Style::new().fg(th.muted);
    let sep = Style::new().fg(th.faint);
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut first = true;
    let mut push_sep = |spans: &mut Vec<Span<'static>>| {
        if !std::mem::take(&mut first) {
            spans.push(Span::styled(" • ", sep));
        }
    };
    for (k, d) in &head {
        push_sep(&mut spans);
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(format!(" {d}"), desc));
    }
    if cut && avail >= 1 {
        push_sep(&mut spans);
        spans.push(Span::styled("…", sep));
    }
    for (k, d) in tail {
        push_sep(&mut spans);
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(format!(" {d}"), desc));
    }
    Line::from(truncate_spans(spans, width))
}

/// Text input with a block cursor (huh's text input).
pub fn input_spans(before: &str, after: &str, style: Style) -> Vec<Span<'static>> {
    let th = t();
    let cursor = if th.has_color() {
        Style::new().bg(th.accent).fg(th.ink)
    } else {
        Style::new().add_modifier(Modifier::REVERSED)
    };
    let mut chars = after.chars();
    let at = chars.next();
    let mut v = vec![Span::styled(before.to_string(), style)];
    match at {
        Some(c) => {
            v.push(Span::styled(c.to_string(), cursor));
            v.push(Span::styled(chars.collect::<String>(), style));
        }
        None => v.push(Span::styled(" ", cursor)),
    }
    v
}

/// `text` as spans in `base`, with the parts matching any of `words`
/// (case-insensitively) underlined: bubbles list's filter highlight.
pub fn highlight(text: &str, words: &[String], base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<String> = chars.iter().map(|c| c.to_lowercase().collect()).collect();
    let mut hit = vec![false; chars.len()];
    for w in words {
        let wl: Vec<String> = w.chars().map(|c| c.to_lowercase().collect()).collect();
        if wl.is_empty() || wl.len() > lower.len() {
            continue;
        }
        for start in 0..=lower.len() - wl.len() {
            if lower[start..start + wl.len()] == wl[..] {
                hit[start..start + wl.len()]
                    .iter_mut()
                    .for_each(|h| *h = true);
            }
        }
    }
    let mark = base.add_modifier(Modifier::UNDERLINED);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    for (c, h) in chars.into_iter().zip(hit) {
        if h != run_hit && !run.is_empty() {
            out.push(Span::styled(
                std::mem::take(&mut run),
                if run_hit { mark } else { base },
            ));
        }
        run_hit = h;
        run.push(c);
    }
    if !run.is_empty() {
        out.push(Span::styled(run, if run_hit { mark } else { base }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn help_line_cuts_at_an_entry() {
        let main = [("j/k", "移動"), ("enter", "開く"), ("c", "チャット")];
        let tail = [("?", "ヘルプ"), ("q", "終了")];
        assert_eq!(
            text(&help_line(&main, &tail, 80)),
            " j/k 移動 • enter 開く • c チャット • ? ヘルプ • q 終了"
        );
        // The end of `main` goes first; the tail stays.
        assert_eq!(
            text(&help_line(&main, &tail, 40)),
            " j/k 移動 • … • ? ヘルプ • q 終了"
        );
        // Not even the tail fits: cut everything in order.
        assert_eq!(text(&help_line(&main, &tail, 20)), " j/k 移動 • …");
        for w in 0..70 {
            let l = text(&help_line(&main, &tail, w));
            assert!(str_width(&l) <= w, "{w}: {l:?}");
        }
    }

    #[test]
    fn line_lr_drops_the_right_side_when_narrow() {
        let l = line_lr(vec![Span::raw("左")], vec![Span::raw("右")], 10);
        assert_eq!(text(&l), "左      右");
        let l = line_lr(vec![Span::raw("ながいタイトル")], vec![Span::raw("右")], 10);
        assert_eq!(text(&l), "ながいタ…");
        assert!(str_width(&text(&l)) <= 10);
    }

    #[test]
    fn truncate_spans_keeps_styles() {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let v = truncate_spans(vec![Span::styled("abc", bold), Span::raw("defgh")], 5);
        assert_eq!(
            v.iter().map(|s| s.content.as_ref()).collect::<String>(),
            "abcd…"
        );
        assert_eq!(v[0].style, bold);
        assert!(truncate_spans(vec![Span::raw("abc")], 0).is_empty());
    }

    #[test]
    fn highlight_marks_matches() {
        let v = highlight("Login 画面", &["log".into(), "画".into()], Style::new());
        let marked: Vec<(&str, bool)> = v
            .iter()
            .map(|s| {
                (
                    s.content.as_ref(),
                    s.style.add_modifier.contains(Modifier::UNDERLINED),
                )
            })
            .collect();
        assert_eq!(
            marked,
            vec![("Log", true), ("in ", false), ("画", true), ("面", false)]
        );
    }

    #[test]
    fn input_spans_cursor_at_end() {
        let v = input_spans("abc", "", Style::new());
        assert_eq!(v.last().map(|s| s.content.as_ref()), Some(" "));
        let v = input_spans("a", "bc", Style::new());
        assert_eq!(v[1].content, "b");
        assert_eq!(v[2].content, "c");
    }
}
