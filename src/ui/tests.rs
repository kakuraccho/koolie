//! Rendering tests on ratatui's TestBackend, against the demo fleet.

use super::*;
use crate::app::{AppMsg, ConnectorFactory, DetailMode};
use crate::config::{Config, HostConfig};
use crate::session::Connector;
use crate::transcript::{Conversation, Step, Turn};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

fn press(c: KeyCode) -> AppMsg {
    AppMsg::Input(Event::Key(KeyEvent::new(c, KeyModifiers::NONE)))
}

async fn demo_app() -> (App, mpsc::UnboundedReceiver<AppMsg>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut cfg = Config::default();
    cfg.notify.sinks.clear();
    let path = std::env::temp_dir().join(format!(
        "koolie-ui-test-{}-{:?}.toml",
        std::process::id(),
        std::thread::current().id()
    ));
    let factory: ConnectorFactory = Arc::new(|h: &HostConfig| {
        Arc::new(crate::demo::DemoConnector::new(&h.name)) as Arc<dyn Connector>
    });
    let mut app = App::new(
        cfg,
        path,
        crate::demo::hosts(),
        factory,
        tx,
        time::UtcOffset::UTC,
    );
    while app.hosts.iter().any(|h| h.inventory.is_none()) {
        let m = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        app.handle(m);
    }
    (app, rx)
}

/// The screen as text, one string per row (wide chars once).
fn render(app: &App, w: u16, h: u16) -> Vec<String> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| {
            let mut row = String::new();
            let mut skip = 0;
            for x in 0..w {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = buf[(x, y)].symbol();
                skip = UnicodeWidthStr::width(sym).saturating_sub(1);
                row.push_str(sym);
            }
            row
        })
        .collect()
}

#[tokio::test]
async fn console_draws_the_list_like_bubbles() {
    let (app, _rx) = demo_app().await;
    let screen = render(&app, 100, 30);
    assert!(screen[0].contains(" koolie "), "{:?}", screen[0]);
    assert!(screen[0].contains("入力待ち 1"));
    assert!(screen[0].contains("ホスト 3"));
    assert!(screen[2].contains("5 件"), "{:?}", screen[2]);
    // The first (selected) item has the bar on both of its lines.
    let first = screen
        .iter()
        .position(|l| l.contains("ログイン画面のリファクタ"))
        .unwrap();
    assert!(
        screen[first].trim_start().starts_with("│ ●"),
        "{:?}",
        screen[first]
    );
    assert!(
        screen[first + 1].contains("│   入力待ち"),
        "{:?}",
        screen[first + 1]
    );
    assert!(screen[first].trim_end().ends_with("laptop"));
    // Others have no bar, and a blank line between items while they fit.
    assert!(screen[first + 2].trim().is_empty());
    assert!(!screen[first + 3].contains('│'));
    let last = screen.last().unwrap();
    assert!(last.contains("enter 開く"), "{last:?}");
    assert!(last.contains(" • "));
}

#[tokio::test]
async fn console_packs_items_when_they_do_not_fit() {
    let (app, _rx) = demo_app().await;
    // 5 items × 3 lines do not fit in 12 rows, so no blank lines.
    let screen = render(&app, 80, 16);
    let first = screen
        .iter()
        .position(|l| l.contains("ログイン画面のリファクタ"))
        .unwrap();
    assert!(
        screen[first + 2].contains("flake の依存更新"),
        "{screen:#?}"
    );
}

#[tokio::test]
async fn console_search_underlines_matches() {
    let (mut app, _rx) = demo_app().await;
    app.handle(press(KeyCode::Char('/')));
    for c in "ログ".chars() {
        app.handle(press(KeyCode::Char(c)));
    }
    let screen = render(&app, 100, 30);
    assert!(screen[2].contains("検索 ログ"), "{:?}", screen[2]);
    assert!(screen[2].contains("2 件"), "{:?}", screen[2]);
    assert!(screen.last().unwrap().contains("esc 取消"));
}

#[tokio::test]
async fn every_view_draws_at_every_size() {
    let (mut app, _rx) = demo_app().await;
    type Action = Box<dyn Fn(&mut App)>;
    let mut views: Vec<Action> = vec![
        Box::new(|_| {}),
        Box::new(|a| a.handle(press(KeyCode::Enter))),
        Box::new(|a| {
            a.handle(press(KeyCode::Esc));
            a.handle(press(KeyCode::Tab));
        }),
        Box::new(|a| a.handle(press(KeyCode::Char('?')))),
        Box::new(|a| {
            a.handle(press(KeyCode::Esc));
            a.handle(press(KeyCode::Esc));
            a.handle(press(KeyCode::Esc));
            a.handle(press(KeyCode::Char('H')));
        }),
        Box::new(|a| a.handle(press(KeyCode::Char('a')))),
        Box::new(|a| {
            a.handle(press(KeyCode::Enter));
            a.handle(press(KeyCode::Char('x')));
        }),
        Box::new(|a| {
            a.handle(press(KeyCode::Esc));
            a.handle(press(KeyCode::Esc));
            a.handle(press(KeyCode::Esc));
            a.handle(press(KeyCode::Char('u')));
        }),
    ];
    for step in views.iter_mut() {
        step(&mut app);
        for (w, h) in [
            (1, 1),
            (2, 2),
            (8, 3),
            (20, 5),
            (29, 8),
            (30, 9),
            (40, 12),
            (80, 24),
            (160, 24),
        ] {
            let screen = render(&app, w, h);
            for row in &screen {
                assert!(row.width() <= w as usize, "{:?} {w}x{h}: {row:?}", app.view);
            }
        }
    }
}

#[tokio::test]
async fn help_uses_two_columns_on_short_wide_terminals() {
    let (mut app, _rx) = demo_app().await;
    app.handle(press(KeyCode::Char('?')));
    let tall = render(&app, 100, 40);
    let wide = render(&app, 160, 24);
    let row_with = |s: &[String], k: &str| s.iter().position(|l| l.contains(k));
    // Tall: one column, sections stacked.
    assert!(row_with(&tall, "ホスト管理") > row_with(&tall, "詳細"));
    // Short and wide: the second column starts at the top.
    assert_eq!(row_with(&wide, "ホスト管理"), row_with(&wide, "一覧"));
}

#[tokio::test]
async fn detail_shows_status_pill_and_tabs() {
    let (mut app, _rx) = demo_app().await;
    app.handle(press(KeyCode::Enter));
    let screen = render(&app, 100, 30);
    let info = &screen[2];
    assert!(info.contains("● 入力待ち"), "{info:?}");
    assert!(info.trim_end().ends_with("画面   チャット"), "{info:?}");
    assert!(
        screen
            .iter()
            .any(|l| l.contains(" 入力待ち  画面の質問に 1-9"))
    );
    if let Some(d) = app.detail.as_mut() {
        d.mode = DetailMode::Chat;
    }
    assert!(
        render(&app, 100, 30)
            .iter()
            .any(|l| l.contains("╭ チャット "))
    );
}

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
    let text: Vec<String> = chat_lines(&c, 40, '◐')
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert_eq!(text[0], "┃ 直して");
    assert!(text.contains(&"  ⋯ ツール 1 回".to_string()));
    assert!(text.contains(&"  直しました".to_string()));
    assert!(text.contains(&"  ⚙ Read: src/main.rs".to_string()));
    assert!(!text.contains(&"  見ています".to_string()));
    assert_eq!(text.last().unwrap(), "  ◐ 作業中…");
}

#[test]
fn chat_answers_render_markdown() {
    let c = Conversation {
        turns: vec![Turn {
            prompt: Some("どう?".into()),
            answer: Some("## 結果\n- `cargo test` が **通りました**".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let text: Vec<String> = chat_lines(&c, 40, '◐')
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert_eq!(text[1..], ["  結果", "  • cargo test が 通りました"]);
}

#[test]
fn token_format() {
    assert_eq!(usage::fmt_tokens(999), "999");
    assert_eq!(usage::fmt_tokens(1500), "1.5k");
    assert_eq!(usage::fmt_tokens(2_500_000), "2.5M");
}

fn line_texts(lines: &[ratatui::text::Line]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn thirty_days() -> Vec<(String, f64)> {
    let mut days: Vec<(String, f64)> = (1..=30).map(|d| (format!("2026-11-{d:02}"), 0.0)).collect();
    days[28].1 = 80.0; // the peak, 11/29
    days[29].1 = 10.0; // today, 11/30
    days
}

#[test]
fn daily_chart_scales_columns_to_the_peak() {
    let text = line_texts(&usage::daily_chart(&thirty_days(), 100, 4));
    // Four rows of columns, the axis, the dates, the stats.
    assert_eq!(text.len(), 7, "{text:#?}");
    assert!(text[0].starts_with("$80.00 │"), "{:?}", text[0]);
    assert!(text[3].starts_with("    $0 │"), "{:?}", text[3]);
    // The peak fills every row; today (1/8 of it) half of the bottom row.
    for row in &text[..4] {
        assert!(row.contains("██"), "{row:?}");
    }
    assert!(text[0].trim_end().ends_with("██"), "{:?}", text[0]);
    assert!(text[3].trim_end().ends_with("██ ▄▄"), "{:?}", text[3]);
    // Oldest on the left, today on the right.
    assert!(text[5].trim_start().starts_with("11/01"), "{:?}", text[5]);
    assert!(text[5].trim_end().ends_with("今日"), "{:?}", text[5]);
    assert!(text[6].contains("合計 $90.00"), "{:?}", text[6]);
    assert!(text[6].contains("最大 $80.00 (11/29)"), "{:?}", text[6]);
    assert!(text[6].contains("今日 $10.00"), "{:?}", text[6]);
    for row in &text {
        assert!(row.width() <= 100, "{row:?}");
    }
}

#[test]
fn daily_chart_keeps_the_latest_days_when_narrow() {
    for w in [20, 30, 40, 60, 90] {
        let text = line_texts(&usage::daily_chart(&thirty_days(), w, 3));
        for row in &text[..4] {
            assert!(row.width() < w, "{w}: {row:?}");
        }
        // Today's column is always the last one.
        assert!(
            text[2].trim_end().ends_with('▂') || text[2].trim_end().ends_with('▃'),
            "{w}: {:?}",
            text[2]
        );
    }
}
