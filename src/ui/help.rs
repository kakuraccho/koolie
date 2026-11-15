//! The `?` overlay: every key, in sections (bubbles help's full view). Two
//! columns when one does not fit the height.

use super::centered;
use super::text::str_width;
use super::theme::t;
use super::widgets::truncate_spans;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "一覧",
        &[
            ("↑↓ / j k", "移動"),
            ("enter / c", "詳細を開く / チャットで開く"),
            ("a", "エージェントの端末に入る（ctrl+b → q で戻る）"),
            ("/", "検索"),
            ("n b d w i", "絞り込み: 要対応/入力待ち/完了/作業中/待機"),
            ("h", "ホストで絞り込み"),
            ("H / u", "ホスト管理 / 使用量"),
            ("r", "すべて更新"),
        ],
    ),
    (
        "詳細",
        &[
            ("i", "メッセージを書く（enter 送信、alt+enter 改行）"),
            ("tab", "画面 ⇄ チャット"),
            ("1-9 y n ↑↓ enter", "入力待ちのとき、そのキーを直接送る"),
            ("p", "直接操作（キーをすべて送る。ctrl+g で戻る）"),
            ("x", "中断（Esc を送る）"),
            ("ctrl+t / s", "スニペット"),
            ("esc", "入力欄から操作モードへ / 一覧へ戻る"),
        ],
    ),
    ("ホスト管理", &[("a / r / x", "追加 / 再接続 / 削除")]),
    ("使用量", &[("tab ←→ / r", "ホスト切り替え / 再集計")]),
    (
        "共通",
        &[
            ("esc / q", "前の画面へ（一覧では q で終了）"),
            ("ctrl+c", "終了"),
        ],
    ),
];

const KEY_W: usize = 18;

type Sections<'a> = &'a [(&'a str, &'a [(&'a str, &'a str)])];

/// The sections as lines; `gap` puts a blank line between them.
fn section_lines(sections: Sections, width: usize, gap: bool) -> Vec<Line<'static>> {
    let th = t();
    let mut out = Vec::new();
    for (i, (title, keys)) in sections.iter().enumerate() {
        if i > 0 && gap {
            out.push(Line::raw(""));
        }
        out.push(Line::styled(
            title.to_string(),
            Style::new().fg(th.accent).add_modifier(Modifier::BOLD),
        ));
        for (k, v) in keys.iter() {
            let pad = KEY_W.saturating_sub(str_width(k));
            out.push(Line::from(truncate_spans(
                vec![
                    Span::styled(
                        format!("  {k}{}", " ".repeat(pad)),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(v.to_string(), Style::new().fg(th.gray)),
                ],
                width,
            )));
        }
    }
    out
}

fn height(sections: Sections, gap: bool) -> usize {
    let gaps = if gap {
        sections.len().saturating_sub(1)
    } else {
        0
    };
    sections.iter().map(|(_, k)| k.len() + 1).sum::<usize>() + gaps
}

pub(super) fn draw(f: &mut Frame, area: Rect) {
    let th = t();
    let col_w: usize = 68;
    let wide = area.width as usize >= 2 * col_w + 8;
    let rows = |two: bool, gap: bool| {
        if two {
            height(&SECTIONS[..2], gap).max(height(&SECTIONS[2..], gap))
        } else {
            height(SECTIONS, gap)
        }
    };
    // The roomiest layout that fits: one column, then two, then without the
    // blank lines between sections, then without padding.
    let mut layout = (false, false, 0);
    for (two, gap, pad) in [
        (false, true, 1),
        (true, true, 1),
        (false, false, 1),
        (true, false, 1),
        (false, false, 0),
        (true, false, 0),
    ] {
        if two && !wide {
            continue;
        }
        if rows(two, gap) + 2 + 2 * pad <= area.height as usize {
            layout = (two, gap, pad);
            break;
        }
    }
    let (two_cols, gap, pad) = layout;
    let w = if two_cols { 2 * col_w + 8 } else { col_w + 6 };
    let h = rows(two_cols, gap) + 2 + 2 * pad;
    let r = centered(area, w as u16, h as u16);
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(th.brand))
        .padding(Padding::new(2, 2, pad as u16, pad as u16))
        .title(Span::styled(
            " ヘルプ ",
            Style::new().fg(th.brand).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " 何かキーを押すと閉じます ",
                Style::new().fg(th.subtle),
            ))
            .right_aligned(),
        );
    let inner = block.inner(r);
    f.render_widget(block, r);
    if two_cols {
        let half = inner.width / 2;
        let left = Rect {
            width: half,
            ..inner
        };
        let right = Rect {
            x: inner.x + half + 2,
            width: inner.width.saturating_sub(half + 2),
            ..inner
        };
        f.render_widget(
            Paragraph::new(section_lines(&SECTIONS[..2], left.width as usize, gap)),
            left,
        );
        f.render_widget(
            Paragraph::new(section_lines(&SECTIONS[2..], right.width as usize, gap)),
            right,
        );
    } else {
        f.render_widget(
            Paragraph::new(section_lines(SECTIONS, inner.width as usize, gap)),
            inner,
        );
    }
}
