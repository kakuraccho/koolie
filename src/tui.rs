//! Terminal setup, the input thread and the main loop.

use crate::app::{App, AppMsg};
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

/// Reads terminal events on a plain thread so it can be paused completely
/// while a child process (attach) owns the terminal.
pub struct InputThread {
    paused: Arc<AtomicBool>,
    idle: Arc<AtomicBool>,
}

impl InputThread {
    pub fn spawn(tx: mpsc::UnboundedSender<AppMsg>) -> InputThread {
        let paused = Arc::new(AtomicBool::new(false));
        let idle = Arc::new(AtomicBool::new(false));
        let (p, i) = (paused.clone(), idle.clone());
        std::thread::Builder::new()
            .name("koolie-input".into())
            .spawn(move || {
                loop {
                    if p.load(Ordering::SeqCst) {
                        i.store(true, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    i.store(false, Ordering::SeqCst);
                    match event::poll(Duration::from_millis(50)) {
                        Ok(true) => match event::read() {
                            Ok(ev) => {
                                if tx.send(AppMsg::Input(ev)).is_err() {
                                    return;
                                }
                            }
                            Err(_) => return,
                        },
                        Ok(false) => {}
                        Err(_) => return,
                    }
                }
            })
            .expect("spawn input thread");
        InputThread { paused, idle }
    }

    /// Stop reading and wait until the thread is parked.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        for _ in 0..50 {
            if self.idle.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }
}

type Term = Terminal<CrosstermBackend<std::io::Stdout>>;

fn enter() -> std::io::Result<Term> {
    terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        cursor::Hide
    )?;
    Terminal::new(CrosstermBackend::new(std::io::stdout()))
}

fn leave() {
    let mut out = std::io::stdout();
    let _ = execute!(
        out,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}

/// Run the TUI until the user quits.
pub async fn run(
    mut app: App,
    mut rx: mpsc::UnboundedReceiver<AppMsg>,
    tx: mpsc::UnboundedSender<AppMsg>,
) -> anyhow::Result<()> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        hook(info);
    }));
    let mut term = enter()?;
    let input = InputThread::spawn(tx.clone());
    let ticker = {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            loop {
                iv.tick().await;
                if tx.send(AppMsg::Tick).is_err() {
                    return;
                }
            }
        })
    };
    let result = async {
        loop {
            term.draw(|f| crate::ui::draw(f, &app))?;
            let Some(msg) = rx.recv().await else { break };
            app.handle(msg);
            // Drain what is already queued before drawing again.
            while let Ok(m) = rx.try_recv() {
                app.handle(m);
                if app.quit {
                    break;
                }
            }
            if app.quit {
                break;
            }
            if app.take_bell() {
                let _ = std::io::stdout().write_all(b"\x07");
                let _ = std::io::stdout().flush();
            }
            if let Some(argv) = app.take_attach() {
                input.pause();
                leave();
                println!(
                    "\r\nkoolie: {} を実行します（戻るときは herdr のデタッチ操作: 通常は Ctrl+B → q）\r",
                    argv.join(" ")
                );
                let status = tokio::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .status()
                    .await;
                term = enter()?;
                term.clear()?;
                // Throw away keys typed while detached.
                while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                    let _ = event::read();
                }
                input.resume();
                match status {
                    Ok(s) if s.success() => app.toast("戻りました"),
                    Ok(s) => app.toast(format!("端末を終了しました ({s})")),
                    Err(e) => app.toast(format!("起動できませんでした: {e}")),
                }
                for h in &app.hosts {
                    h.handle.refresh();
                }
            }
        }
        anyhow::Ok(())
    }
    .await;
    ticker.abort();
    input.pause();
    leave();
    app.shutdown().await;
    result
}
