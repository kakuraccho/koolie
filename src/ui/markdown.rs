//! A light Markdown look for agents' answers in Chat, after the spirit of
//! Charm's Glamour: headings, fenced code, lists, quotes, rules, `code` and
//! **bold**. It works line by line and never fails; anything it does not
//! know stays as written.

use super::text::{str_width, truncate, wrap};
use super::theme::t;
use super::widgets::pill;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// `text` as lines of `width` columns, each starting with `indent`.
pub fn render(text: &str, width: usize, indent: &str) -> Vec<Line<'static>> {
    let th = t();
    let iw = str_width(indent);
    let avail = width.saturating_sub(iw).max(1);
    let code = Style::new().fg(th.bar_fg);
    let faint = Style::new().fg(th.faint);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut fence: Option<String> = None;
    let start = |spans: Vec<Span<'static>>| {
        let mut v = vec![Span::raw(indent.to_string())];
        v.extend(spans);
        Line::from(v)
    };
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if let Some(marker) = fence_marker(trimmed) {
            match &fence {
                None => {
                    fence = Some(marker.to_string());
                    let lang = trimmed[marker.len()..].trim();
                    if !lang.is_empty() {
                        out.push(start(vec![Span::styled(
                            truncate(lang, avail),
                            Style::new().fg(th.subtle).add_modifier(Modifier::ITALIC),
                        )]));
                    }
                    continue;
                }
                // Closed by a run of the same char at least as long.
                Some(open)
                    if marker.starts_with(open.as_str())
                        && trimmed[marker.len()..].trim().is_empty() =>
                {
                    fence = None;
                    continue;
                }
                Some(_) => {}
            }
        }
        if fence.is_some() {
            // Code is not wrapped: cut it, like a terminal would.
            out.push(start(vec![
                Span::styled("│ ", faint),
                Span::styled(
                    truncate(&raw.replace('\t', "    "), avail.saturating_sub(2)),
                    code,
                ),
            ]));
            continue;
        }
        if let Some((level, title)) = heading(trimmed) {
            let title = truncate(title, avail.saturating_sub(2));
            out.push(start(vec![if level == 1 {
                pill(&title, th.brand, th.cream)
            } else {
                Span::styled(
                    title,
                    Style::new().fg(th.indigo).add_modifier(Modifier::BOLD),
                )
            }]));
            continue;
        }
        if is_rule(trimmed) {
            out.push(start(vec![Span::styled("─".repeat(avail.min(40)), faint)]));
            continue;
        }
        let lead = raw.len() - trimmed.len();
        let pad = " ".repeat(str_width(&raw[..lead]));
        // (marker, its style, rest of the line, text style, quote?)
        let (mark, mark_style, body, base, quote) = if let Some(rest) = bullet(trimmed) {
            (
                format!("{pad}• "),
                Style::new().fg(th.accent_dim),
                rest,
                Style::new(),
                false,
            )
        } else if let Some((n, rest)) = numbered(trimmed) {
            (
                format!("{pad}{n} "),
                Style::new().fg(th.accent_dim),
                rest,
                Style::new(),
                false,
            )
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            (
                "│ ".to_string(),
                faint,
                rest.trim_start(),
                Style::new().fg(th.muted).add_modifier(Modifier::ITALIC),
                true,
            )
        } else {
            (pad, Style::new(), trimmed, Style::new(), false)
        };
        let mw = str_width(&mark);
        // Wrapped lines hang under the text; a quote keeps its bar.
        let hang = if quote { mark.clone() } else { " ".repeat(mw) };
        let mut state = Inline::default();
        for (i, l) in wrap(body, avail.saturating_sub(mw).max(1))
            .into_iter()
            .enumerate()
        {
            let mut v = vec![Span::styled(
                if i == 0 { mark.clone() } else { hang.clone() },
                mark_style,
            )];
            v.extend(state.spans(&l, base));
            out.push(start(v));
        }
    }
    out
}

/// The run of 3+ backticks or tildes opening a fence, if `line` is one.
fn fence_marker(line: &str) -> Option<&str> {
    for ch in ['`', '~'] {
        let n = line.chars().take_while(|c| *c == ch).count();
        if n >= 3 {
            return Some(&line[..n]);
        }
    }
    None
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let n = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&n) {
        let rest = &line[n..];
        if let Some(title) = rest.strip_prefix(' ') {
            return Some((n, title.trim().trim_end_matches('#').trim_end()));
        }
    }
    None
}

fn is_rule(line: &str) -> bool {
    let l: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    l.len() >= 3
        && (l.chars().all(|c| c == '-')
            || l.chars().all(|c| c == '*')
            || l.chars().all(|c| c == '_'))
}

fn bullet(line: &str) -> Option<&str> {
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|m| line.strip_prefix(m))
        .map(|rest| rest.trim_start())
}

fn numbered(line: &str) -> Option<(&str, &str)> {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((&line[..digits + 1], rest.trim_start()))
}

/// Inline state carried across the wrapped lines of one paragraph.
#[derive(Default)]
struct Inline {
    code: bool,
    bold: bool,
}

impl Inline {
    /// One wrapped line: `` ` `` toggles code, `**` toggles bold; the
    /// markers themselves are not shown.
    fn spans(&mut self, line: &str, base: Style) -> Vec<Span<'static>> {
        let th = t();
        let mut out = Vec::new();
        let mut cur = String::new();
        let style = |s: &Inline| {
            if s.code {
                Style::new().fg(th.pink)
            } else if s.bold {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            }
        };
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            let toggle = if c == '`' {
                Some(true)
            } else if c == '*' && !self.code && chars.peek() == Some(&'*') {
                chars.next();
                Some(false)
            } else {
                None
            };
            match toggle {
                Some(code) => {
                    if !cur.is_empty() {
                        out.push(Span::styled(std::mem::take(&mut cur), style(self)));
                    }
                    if code {
                        self.code = !self.code;
                    } else {
                        self.bold = !self.bold;
                    }
                }
                None => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(Span::styled(cur, style(self)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn renders_blocks() {
        let md = "# 結果\n\n直しました。\n\n## 変更点\n- `src/a.rs` を **修正**\n  - 入れ子\n1. 最初\n> 引用\n---\n```rust\nfn main() {}\n```\n終わり";
        assert_eq!(
            texts(&render(md, 40, "  ")),
            vec![
                "   結果 ",
                "  ",
                "  直しました。",
                "  ",
                "  変更点",
                "  • src/a.rs を 修正",
                "    • 入れ子",
                "  1. 最初",
                "  │ 引用",
                &format!("  {}", "─".repeat(38)),
                "  rust",
                "  │ fn main() {}",
                "  終わり",
            ]
        );
    }

    #[test]
    fn inline_styles_carry_across_wrapped_lines() {
        let lines = render("**ここは太字のままで続きます**", 10, "");
        assert!(lines.len() > 1);
        for l in &lines {
            for s in l.spans.iter().filter(|s| !s.content.is_empty()) {
                assert!(s.style.add_modifier.contains(Modifier::BOLD), "{l:?}");
            }
        }
    }

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(texts(&render("a * b # c", 40, "")), vec!["a * b # c"]);
        assert_eq!(texts(&render("#tag", 40, "")), vec!["#tag"]);
        // An unclosed fence keeps the rest as code instead of failing.
        assert_eq!(texts(&render("```\nx", 40, "")), vec!["│ x"]);
    }

    #[test]
    fn lines_fit_the_width() {
        let md = "# とても長い見出しがここに入ります\n- リストの項目もとても長くて折り返します\n```\nコードの行は折り返さずに切ります。長い長い長い\n```";
        for w in 4..30 {
            for l in texts(&render(md, w, "  ")) {
                assert!(str_width(&l) <= w.max(6), "{w}: {l:?}");
            }
        }
    }
}
