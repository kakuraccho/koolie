//! The TUI's state and behaviour, independent of drawing.

use crate::backend::{Backend, ReadSource, Screen};
use crate::config::{Config, HostConfig, Sink};
use crate::discover::{self, Candidate, ProbeResult};
use crate::model::{Agent, AgentKey, AgentStatus, HostId, Inventory, ServerKind, console_order};
use crate::notify::{self, Tracker};
use crate::session::{self, ConnState, Connector, HostConnector, HostHandle, HostUpdate};
use crate::transcript::{self, Conversation, Follower, Harness};
use crate::transport::{SshTransport, Transport};
use crate::ui::text::TextInput;
use crate::usage::UsageReport;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const PEEK_INTERVAL: Duration = Duration::from_millis(700);
const CHAT_INTERVAL: Duration = Duration::from_millis(1500);
const PEEK_LINES: u32 = 400;

/// Messages into the app loop.
#[derive(Debug)]
pub enum AppMsg {
    Host(HostUpdate),
    Input(Event),
    Screen {
        key: AgentKey,
        result: Result<Screen, String>,
    },
    Chat {
        key: AgentKey,
        result: Result<Conversation, String>,
    },
    /// An action on an agent finished.
    Action {
        key: AgentKey,
        ok: bool,
        message: String,
        clear_draft: bool,
    },
    Usage {
        host: HostId,
        result: Result<UsageReport, String>,
    },
    Candidates(Vec<Candidate>),
    Probed {
        target: String,
        result: Result<ProbeResult, String>,
    },
    Tick,
}

/// One configured host as the app sees it.
#[derive(Debug)]
pub struct HostEntry {
    pub cfg: HostConfig,
    pub handle: HostHandle,
    pub state: ConnState,
    pub inventory: Option<Inventory>,
    pub version: Option<String>,
    pub route: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Console,
    Detail,
    Hosts,
    Usage,
    Help,
}

/// Console status filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    /// Blocked and done.
    NeedsYou,
    Only(AgentStatus),
}

impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "すべて",
            Filter::NeedsYou => "要対応",
            Filter::Only(AgentStatus::Blocked) => "入力待ち",
            Filter::Only(AgentStatus::Done) => "完了",
            Filter::Only(AgentStatus::Working) => "作業中",
            Filter::Only(AgentStatus::Idle) => "待機",
            Filter::Only(AgentStatus::Unknown) => "不明",
        }
    }
    fn matches(self, s: AgentStatus) -> bool {
        match self {
            Filter::All => true,
            Filter::NeedsYou => s.is_attention(),
            Filter::Only(x) => x == s,
        }
    }
}

#[derive(Debug, Default)]
pub struct ConsoleState {
    pub selected: usize,
    pub filter: Filter,
    pub host_filter: Option<HostId>,
    pub search: String,
    pub searching: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailMode {
    Peek,
    Chat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    View,
    Compose,
}

#[derive(Debug)]
pub struct DetailState {
    pub key: AgentKey,
    pub mode: DetailMode,
    pub focus: Focus,
    pub screen: Option<Screen>,
    pub screen_error: Option<String>,
    /// Lines scrolled up from the bottom.
    pub scroll: usize,
    pub chat: Option<Conversation>,
    pub chat_error: Option<String>,
    pub chat_scroll: usize,
    pub composer: TextInput,
    /// An action in flight (prevents double sends).
    pub busy: bool,
    pub snippet_picker: Option<usize>,
    poller: Option<JoinHandle<()>>,
    chat_poller: Option<JoinHandle<()>>,
    /// The agent session the chat poller follows.
    chat_session: Option<crate::model::AgentSession>,
}

impl Drop for DetailState {
    fn drop(&mut self) {
        if let Some(p) = self.poller.take() {
            p.abort();
        }
        if let Some(p) = self.chat_poller.take() {
            p.abort();
        }
    }
}

/// Adding a host, step by step.
#[derive(Debug)]
pub enum AddStep {
    Pick {
        selected: usize,
        loading: bool,
    },
    Manual {
        input: TextInput,
    },
    Probing {
        target: String,
    },
    Choose {
        target: String,
        name: TextInput,
        options: Vec<(String, HostConfig, bool)>,
        selected: usize,
    },
}

#[derive(Debug, Default)]
pub struct HostsState {
    pub selected: usize,
    pub add: Option<AddStep>,
    pub candidates: Vec<Candidate>,
    pub confirm_remove: bool,
}

#[derive(Debug, Default)]
pub struct UsageState {
    /// `None` = all hosts.
    pub host: Option<HostId>,
    pub reports: std::collections::BTreeMap<HostId, Result<UsageReport, String>>,
    pub loading: std::collections::BTreeSet<HostId>,
}

pub struct App {
    pub cfg: Config,
    pub cfg_path: PathBuf,
    pub hosts: Vec<HostEntry>,
    pub view: View,
    pub prev_view: View,
    pub console: ConsoleState,
    pub detail: Option<DetailState>,
    pub hosts_view: HostsState,
    pub usage: UsageState,
    pub toast: Option<(String, Instant)>,
    pub tracker: Tracker,
    pub quit: bool,
    pub utc_offset: time::UtcOffset,
    tx: mpsc::UnboundedSender<AppMsg>,
    host_tx: mpsc::UnboundedSender<HostUpdate>,
    bell: bool,
    attach: Option<Vec<String>>,
    spinner: usize,
    connectors: Option<ConnectorFactory>,
}

/// A host session factory, so tests and the demo can inject backends.
pub type ConnectorFactory = Arc<dyn Fn(&HostConfig) -> Arc<dyn Connector> + Send + Sync>;

pub fn real_connectors() -> ConnectorFactory {
    Arc::new(|cfg: &HostConfig| Arc::new(HostConnector::new(cfg.clone())) as Arc<dyn Connector>)
}

impl App {
    pub fn new(
        cfg: Config,
        cfg_path: PathBuf,
        hosts: Vec<HostConfig>,
        connectors: ConnectorFactory,
        tx: mpsc::UnboundedSender<AppMsg>,
        utc_offset: time::UtcOffset,
    ) -> App {
        let (host_tx, mut host_rx) = mpsc::unbounded_channel::<HostUpdate>();
        let fwd = tx.clone();
        tokio::spawn(async move {
            while let Some(u) = host_rx.recv().await {
                if fwd.send(AppMsg::Host(u)).is_err() {
                    return;
                }
            }
        });
        let tracker = Tracker::new(&cfg.notify);
        let mut app = App {
            cfg,
            cfg_path,
            hosts: Vec::new(),
            view: View::Console,
            prev_view: View::Console,
            console: ConsoleState::default(),
            detail: None,
            hosts_view: HostsState::default(),
            usage: UsageState::default(),
            toast: None,
            tracker,
            quit: false,
            utc_offset,
            tx,
            host_tx,
            bell: false,
            attach: None,
            spinner: 0,
            connectors: None,
        };
        app.connectors = Some(connectors);
        for h in hosts {
            app.add_host_session(h);
        }
        app
    }
}

// The factory lives outside the struct literal above to keep `new` readable.
impl App {
    fn connector_for(&self, cfg: &HostConfig) -> Arc<dyn Connector> {
        match &self.connectors {
            Some(f) => f(cfg),
            None => Arc::new(HostConnector::new(cfg.clone())),
        }
    }
}

impl App {
    pub fn add_host_session(&mut self, cfg: HostConfig) {
        let connector = self.connector_for(&cfg);
        let handle = session::spawn(cfg.name.clone(), cfg.kind, connector, self.host_tx.clone());
        self.hosts.push(HostEntry {
            cfg,
            handle,
            state: ConnState::Connecting,
            inventory: None,
            version: None,
            route: String::new(),
        });
    }

    pub fn host(&self, id: &str) -> Option<&HostEntry> {
        self.hosts.iter().find(|h| h.cfg.name == id)
    }

    fn host_mut(&mut self, id: &str) -> Option<&mut HostEntry> {
        self.hosts.iter_mut().find(|h| h.cfg.name == id)
    }

    pub fn spinner(&self) -> char {
        const S: [char; 4] = ['⠋', '⠙', '⠹', '⠸'];
        S[self.spinner % S.len()]
    }

    /// Every known agent, Console-ordered.
    pub fn all_agents(&self) -> Vec<&Agent> {
        let mut v: Vec<&Agent> = self
            .hosts
            .iter()
            .filter_map(|h| h.inventory.as_ref())
            .flat_map(|i| i.agents.iter())
            .collect();
        v.sort_by(|a, b| console_order(a, b));
        v
    }

    /// Agents shown by the Console with the current filters.
    pub fn rows(&self) -> Vec<&Agent> {
        let q = self.console.search.to_lowercase();
        self.all_agents()
            .into_iter()
            .filter(|a| self.console.filter.matches(a.status))
            .filter(|a| {
                self.console
                    .host_filter
                    .as_ref()
                    .is_none_or(|h| &a.key.host == h)
            })
            .filter(|a| {
                q.is_empty() || {
                    let hay = format!(
                        "{} {} {} {} {}",
                        a.program(),
                        a.display_title(),
                        a.key.host,
                        a.workspace_label.as_deref().unwrap_or(""),
                        a.cwd.as_deref().unwrap_or("")
                    )
                    .to_lowercase();
                    q.split_whitespace().all(|w| hay.contains(w))
                }
            })
            .collect()
    }

    pub fn agent(&self, key: &AgentKey) -> Option<&Agent> {
        self.host(&key.host)?
            .inventory
            .as_ref()?
            .agents
            .iter()
            .find(|a| &a.key == key)
    }

    /// Whether this agent's host is connected (actions allowed).
    pub fn is_live(&self, key: &AgentKey) -> bool {
        self.host(&key.host).is_some_and(|h| h.state.is_connected())
    }

    pub fn counts(&self) -> [usize; 4] {
        let mut c = [0; 4];
        for a in self.all_agents() {
            match a.status {
                AgentStatus::Blocked => c[0] += 1,
                AgentStatus::Done => c[1] += 1,
                AgentStatus::Working => c[2] += 1,
                _ => c[3] += 1,
            }
        }
        c
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    pub fn take_attach(&mut self) -> Option<Vec<String>> {
        self.attach.take()
    }

    fn backend(&self, key: &AgentKey) -> Option<Arc<dyn Backend>> {
        self.host(&key.host)?.handle.backend()
    }

    // ---------------------------------------------------------------- updates

    pub fn handle(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Host(u) => self.on_host(u),
            AppMsg::Input(ev) => self.on_input(ev),
            AppMsg::Screen { key, result } => {
                if let Some(d) = self.detail.as_mut().filter(|d| d.key == key) {
                    match result {
                        Ok(s) => {
                            d.screen = Some(s);
                            d.screen_error = None;
                        }
                        Err(e) => d.screen_error = Some(e),
                    }
                }
            }
            AppMsg::Chat { key, result } => {
                if let Some(d) = self.detail.as_mut().filter(|d| d.key == key) {
                    match result {
                        Ok(c) => {
                            d.chat = Some(c);
                            d.chat_error = None;
                        }
                        Err(e) => d.chat_error = Some(e),
                    }
                }
            }
            AppMsg::Action {
                key,
                ok,
                message,
                clear_draft,
            } => {
                if let Some(d) = self.detail.as_mut().filter(|d| d.key == key) {
                    d.busy = false;
                    if ok && clear_draft {
                        d.composer.clear();
                    }
                }
                self.toast(message);
                if let Some(h) = self.host(&key.host) {
                    h.handle.refresh();
                }
            }
            AppMsg::Usage { host, result } => {
                self.usage.loading.remove(&host);
                self.usage.reports.insert(host, result);
            }
            AppMsg::Candidates(c) => {
                self.hosts_view.candidates = c;
                if let Some(AddStep::Pick { loading, .. }) = self.hosts_view.add.as_mut() {
                    *loading = false;
                }
            }
            AppMsg::Probed { target, result } => self.on_probed(target, result),
            AppMsg::Tick => {
                self.spinner = self.spinner.wrapping_add(1);
                if self
                    .toast
                    .as_ref()
                    .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4))
                {
                    self.toast = None;
                }
            }
        }
    }

    fn on_host(&mut self, u: HostUpdate) {
        match u {
            HostUpdate::State { host, state } => {
                let lost = !state.is_connected();
                if lost {
                    self.tracker.reset_host(&host);
                }
                if let Some(h) = self.host_mut(&host) {
                    h.state = state;
                }
            }
            HostUpdate::Connected { host, info, route } => {
                if let Some(h) = self.host_mut(&host) {
                    h.version = Some(info.version);
                    h.route = route;
                }
            }
            HostUpdate::Inventory { host, inventory } => {
                let viewing = (self.view == View::Detail)
                    .then(|| self.detail.as_ref().map(|d| d.key.clone()))
                    .flatten();
                let notes = self.tracker.observe(&host, &inventory, viewing.as_ref());
                for n in &notes {
                    notify::deliver(&self.cfg.notify, n);
                    if self.cfg.notify.sinks.contains(&Sink::Bell) {
                        self.bell = true;
                    }
                }
                if let Some(n) = notes.first() {
                    self.toast(format!("{} — {}", n.summary(), n.title));
                }
                if let Some(h) = self.host_mut(&host) {
                    h.inventory = Some(inventory);
                }
                self.clamp_selection();
                self.restart_chat_if_session_changed();
            }
            HostUpdate::StatusChanged { .. } => {}
        }
    }

    fn clamp_selection(&mut self) {
        let n = self.rows().len();
        if self.console.selected >= n {
            self.console.selected = n.saturating_sub(1);
        }
    }

    // ------------------------------------------------------------------ input

    fn on_input(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
            Event::Paste(s) => self.on_paste(&s),
            _ => {}
        }
    }

    fn on_paste(&mut self, s: &str) {
        match self.view {
            View::Detail => {
                if let Some(d) = self.detail.as_mut() {
                    d.focus = Focus::Compose;
                    d.composer.insert_str(s);
                }
            }
            View::Console if self.console.searching => self.console.search.push_str(s.trim()),
            View::Hosts => match self.hosts_view.add.as_mut() {
                Some(AddStep::Manual { input }) => input.insert_str(s.trim()),
                Some(AddStep::Choose { name, .. }) => name.insert_str(s.trim()),
                _ => {}
            },
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            // Ctrl+C quits from anywhere except the composer, where it clears.
            if let (View::Detail, Some(d)) = (self.view, self.detail.as_mut())
                && d.focus == Focus::Compose
                && !d.composer.is_blank()
            {
                d.composer.clear();
                return;
            }
            self.quit = true;
            return;
        }
        match self.view {
            View::Console => self.key_console(k),
            View::Detail => self.key_detail(k),
            View::Hosts => self.key_hosts(k),
            View::Usage => self.key_usage(k),
            View::Help => {
                self.view = self.prev_view;
            }
        }
    }

    fn key_console(&mut self, k: KeyEvent) {
        if self.console.searching {
            match k.code {
                KeyCode::Esc => {
                    self.console.searching = false;
                    self.console.search.clear();
                }
                KeyCode::Enter => self.console.searching = false,
                KeyCode::Backspace => {
                    self.console.search.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.console.search.push(c)
                }
                _ => {}
            }
            self.console.selected = 0;
            return;
        }
        let n = self.rows().len();
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 {
                    self.console.selected = (self.console.selected + 1).min(n - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.console.selected = self.console.selected.saturating_sub(1)
            }
            KeyCode::Char('g') | KeyCode::Home => self.console.selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.console.selected = n.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                self.open_selected(DetailMode::Peek)
            }
            KeyCode::Char('c') => self.open_selected(DetailMode::Chat),
            KeyCode::Char('a') => {
                if let Some(key) = self.selected_key() {
                    self.request_attach(&key);
                }
            }
            KeyCode::Char('/') => {
                self.console.searching = true;
                self.console.search.clear();
            }
            KeyCode::Esc => {
                self.console.filter = Filter::All;
                self.console.host_filter = None;
                self.console.search.clear();
            }
            KeyCode::Char('n') => self.set_filter(Filter::NeedsYou),
            KeyCode::Char('b') => self.set_filter(Filter::Only(AgentStatus::Blocked)),
            KeyCode::Char('d') => self.set_filter(Filter::Only(AgentStatus::Done)),
            KeyCode::Char('w') => self.set_filter(Filter::Only(AgentStatus::Working)),
            KeyCode::Char('i') => self.set_filter(Filter::Only(AgentStatus::Idle)),
            KeyCode::Char('h') => self.cycle_host_filter(),
            KeyCode::Char('r') => {
                for h in &self.hosts {
                    h.handle.refresh();
                }
                self.toast("更新しました");
            }
            KeyCode::Char('H') => self.view = View::Hosts,
            KeyCode::Char('u') => self.open_usage(),
            KeyCode::Char('?') => self.show_help(),
            _ => {}
        }
    }

    fn set_filter(&mut self, f: Filter) {
        self.console.filter = if self.console.filter == f {
            Filter::All
        } else {
            f
        };
        self.console.selected = 0;
    }

    fn cycle_host_filter(&mut self) {
        let names: Vec<HostId> = self.hosts.iter().map(|h| h.cfg.name.clone()).collect();
        self.console.host_filter = match &self.console.host_filter {
            None => names.first().cloned(),
            Some(cur) => {
                let i = names.iter().position(|n| n == cur);
                i.and_then(|i| names.get(i + 1).cloned())
            }
        };
        self.console.selected = 0;
    }

    fn show_help(&mut self) {
        self.prev_view = self.view;
        self.view = View::Help;
    }

    pub fn selected_key(&self) -> Option<AgentKey> {
        self.rows()
            .get(self.console.selected)
            .map(|a| a.key.clone())
    }

    fn open_selected(&mut self, mode: DetailMode) {
        if let Some(key) = self.selected_key() {
            self.open_detail(key, mode);
        }
    }

    pub fn open_detail(&mut self, key: AgentKey, mode: DetailMode) {
        let blocked = self
            .agent(&key)
            .is_some_and(|a| a.status == AgentStatus::Blocked);
        let mut d = DetailState {
            key: key.clone(),
            mode,
            // Type right away; while blocked, keys answer the dialog instead.
            focus: if blocked { Focus::View } else { Focus::Compose },
            screen: None,
            screen_error: None,
            scroll: 0,
            chat: None,
            chat_error: None,
            chat_scroll: 0,
            composer: TextInput::default(),
            busy: false,
            snippet_picker: None,
            poller: None,
            chat_poller: None,
            chat_session: None,
        };
        d.poller = Some(self.spawn_peek(key.clone()));
        if mode == DetailMode::Chat {
            d.chat_session = self.agent(&key).and_then(|a| a.session.clone());
            d.chat_poller = Some(self.spawn_chat(key));
        }
        self.detail = Some(d);
        self.view = View::Detail;
    }

    fn close_detail(&mut self) {
        self.detail = None;
        self.view = View::Console;
    }

    fn spawn_peek(&self, key: AgentKey) -> JoinHandle<()> {
        let handle = self.host(&key.host).map(|h| h.handle.clone());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let Some(handle) = handle else { return };
            let mut last_rev = None;
            loop {
                if let Some(b) = handle.backend() {
                    let r = b
                        .read(&key.id, ReadSource::Recent, Some(PEEK_LINES), true)
                        .await;
                    let changed = match &r {
                        Ok(s) => last_rev != Some(s.revision) || s.revision == 0,
                        Err(_) => true,
                    };
                    if let Ok(s) = &r {
                        last_rev = Some(s.revision);
                    }
                    if changed
                        && tx
                            .send(AppMsg::Screen {
                                key: key.clone(),
                                result: r.map_err(|e| e.to_string()),
                            })
                            .is_err()
                    {
                        return;
                    }
                }
                tokio::time::sleep(PEEK_INTERVAL).await;
            }
        })
    }

    fn spawn_chat(&self, key: AgentKey) -> JoinHandle<()> {
        let session = self.agent(&key).and_then(|a| a.session.clone());
        let transport: Option<Arc<dyn Transport>> =
            self.host(&key.host).map(|h| h.handle.transport());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let send = |r: Result<Conversation, String>| {
                tx.send(AppMsg::Chat {
                    key: key.clone(),
                    result: r,
                })
                .is_ok()
            };
            let Some(transport) = transport else { return };
            let Some(session) = session else {
                send(Err(chat_error_text(
                    &transcript::TranscriptError::NoSession,
                )));
                return;
            };
            let Some(harness) = Harness::from_agent(&session.agent) else {
                send(Err(chat_error_text(
                    &transcript::TranscriptError::UnsupportedAgent,
                )));
                return;
            };
            let mut follower: Option<Follower> = None;
            let mut sent_once = false;
            loop {
                if follower.is_none() {
                    match transcript::locate(transport.as_ref(), &session).await {
                        Ok(p) => follower = Some(Follower::new(harness, p)),
                        Err(e) => {
                            if !send(Err(chat_error_text(&e))) {
                                return;
                            }
                        }
                    }
                }
                if let Some(f) = follower.as_mut() {
                    match f.poll(transport.as_ref()).await {
                        Ok(changed) => {
                            if (changed || !sent_once) && !send(Ok(f.conversation().clone())) {
                                return;
                            }
                            sent_once = true;
                        }
                        Err(e) => {
                            if e == transcript::TranscriptError::NotFound {
                                follower = None;
                            }
                            if !send(Err(chat_error_text(&e))) {
                                return;
                            }
                        }
                    }
                }
                tokio::time::sleep(CHAT_INTERVAL).await;
            }
        })
    }

    fn key_detail(&mut self, k: KeyEvent) {
        let Some(d) = self.detail.as_mut() else {
            self.view = View::Console;
            return;
        };
        let key = d.key.clone();
        let status = self.agent(&key).map(|a| a.status).unwrap_or_default();
        let d = self.detail.as_mut().expect("checked");

        if let Some(sel) = d.snippet_picker {
            let n = self.cfg.snippets.len();
            match k.code {
                KeyCode::Esc => d.snippet_picker = None,
                KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                    d.snippet_picker = Some((sel + 1).min(n - 1))
                }
                KeyCode::Up | KeyCode::Char('k') => d.snippet_picker = Some(sel.saturating_sub(1)),
                KeyCode::Enter => {
                    if let Some(s) = self.cfg.snippets.get(sel) {
                        d.composer.insert_str(&s.text);
                    }
                    d.snippet_picker = None;
                    d.focus = Focus::Compose;
                }
                _ => {}
            }
            return;
        }

        if d.focus == Focus::Compose {
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            let alt = k.modifiers.contains(KeyModifiers::ALT)
                || k.modifiers.contains(KeyModifiers::SHIFT);
            match k.code {
                KeyCode::Esc => d.focus = Focus::View,
                KeyCode::Enter if alt => d.composer.insert('\n'),
                KeyCode::Char('j') if ctrl => d.composer.insert('\n'),
                KeyCode::Enter => self.send_draft(&key, status),
                KeyCode::Char('t') if ctrl => {
                    if !self.cfg.snippets.is_empty() {
                        d.snippet_picker = Some(0);
                    } else {
                        self.toast("スニペットが設定されていません (config.toml の [[snippet]])");
                    }
                }
                KeyCode::Char('u') if ctrl => d.composer.kill_to_start(),
                KeyCode::Char('w') if ctrl => d.composer.delete_word(),
                KeyCode::Char('a') if ctrl => d.composer.home(),
                KeyCode::Char('e') if ctrl => d.composer.end(),
                KeyCode::Backspace => d.composer.backspace(),
                KeyCode::Delete => d.composer.delete(),
                KeyCode::Left => d.composer.left(),
                KeyCode::Right => d.composer.right(),
                KeyCode::Up => d.composer.vertical(-1),
                KeyCode::Down => d.composer.vertical(1),
                KeyCode::Home => d.composer.home(),
                KeyCode::End => d.composer.end(),
                KeyCode::PageUp => scroll(d, 20),
                KeyCode::PageDown => scroll(d, -20),
                KeyCode::Tab => self.toggle_mode(),
                KeyCode::Char(c) if !ctrl => d.composer.insert(c),
                _ => {}
            }
            return;
        }

        // View focus. While blocked, answer keys go straight to the agent.
        if status == AgentStatus::Blocked {
            let answer: Option<&str> = match k.code {
                KeyCode::Char(c @ '1'..='9') => Some(match c {
                    '1' => "1",
                    '2' => "2",
                    '3' => "3",
                    '4' => "4",
                    '5' => "5",
                    '6' => "6",
                    '7' => "7",
                    '8' => "8",
                    _ => "9",
                }),
                KeyCode::Char('y') => Some("y"),
                KeyCode::Char('n') => Some("n"),
                KeyCode::Enter => Some("enter"),
                KeyCode::Up => Some("up"),
                KeyCode::Down => Some("down"),
                KeyCode::Left => Some("left"),
                KeyCode::Right => Some("right"),
                KeyCode::Tab => Some("tab"),
                _ => None,
            };
            if let Some(key_name) = answer {
                self.send_keys(&key, vec![key_name.to_string()]);
                return;
            }
        }
        let d = self.detail.as_mut().expect("checked");
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_detail(),
            KeyCode::Char('i') | KeyCode::Enter => d.focus = Focus::Compose,
            KeyCode::Tab | KeyCode::Char('c') => self.toggle_mode(),
            KeyCode::Char('a') => self.request_attach(&key),
            KeyCode::Char('x') => self.send_keys(&key, vec!["esc".into()]),
            KeyCode::Char('s') => {
                if !self.cfg.snippets.is_empty() {
                    d.snippet_picker = Some(0);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => scroll(d, 1),
            KeyCode::Down | KeyCode::Char('j') => scroll(d, -1),
            KeyCode::PageUp => scroll(d, 20),
            KeyCode::PageDown => scroll(d, -20),
            KeyCode::Char('G') | KeyCode::End => {
                d.scroll = 0;
                d.chat_scroll = 0;
            }
            KeyCode::Char('?') => self.show_help(),
            _ => {}
        }
    }

    fn toggle_mode(&mut self) {
        let session = self
            .detail
            .as_ref()
            .and_then(|d| self.agent(&d.key))
            .and_then(|a| a.session.clone());
        let chat_poller = match self.detail.as_ref() {
            Some(d) if d.mode == DetailMode::Peek && d.chat_poller.is_none() => {
                Some(self.spawn_chat(d.key.clone()))
            }
            _ => None,
        };
        if let Some(d) = self.detail.as_mut() {
            d.mode = match d.mode {
                DetailMode::Peek => DetailMode::Chat,
                DetailMode::Chat => DetailMode::Peek,
            };
            if chat_poller.is_some() {
                d.chat_poller = chat_poller;
                d.chat_session = session;
            }
        }
    }

    /// Follow the new transcript when the agent started another session
    /// (Claude Code's `/clear`, Codex's new thread).
    fn restart_chat_if_session_changed(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        if d.chat_poller.is_none() {
            return;
        }
        let current = self.agent(&d.key).and_then(|a| a.session.clone());
        if current.is_none() || current == d.chat_session {
            return;
        }
        let handle = self.spawn_chat(d.key.clone());
        if let Some(d) = self.detail.as_mut() {
            if let Some(old) = d.chat_poller.replace(handle) {
                old.abort();
            }
            d.chat_session = current;
            d.chat = None;
            d.chat_error = None;
            d.chat_scroll = 0;
        }
    }

    /// Composer Send: prompt, insert (while blocked) or stop (empty draft
    /// while working).
    fn send_draft(&mut self, key: &AgentKey, status: AgentStatus) {
        let Some(d) = self.detail.as_mut() else {
            return;
        };
        if d.busy {
            self.toast("送信中です");
            return;
        }
        let text = d.composer.text();
        let blank = d.composer.is_blank();
        if !self.is_live(key) {
            self.toast("ホストに接続されていません");
            return;
        }
        let Some(backend) = self.backend(key) else {
            self.toast("ホストに接続されていません");
            return;
        };
        if blank {
            if status == AgentStatus::Working {
                self.send_keys(key, vec!["esc".into()]);
            }
            return;
        }
        let d = self.detail.as_mut().expect("checked");
        d.busy = true;
        let tx = self.tx.clone();
        let key = key.clone();
        let text = text.trim_end_matches('\n').to_string();
        tokio::spawn(async move {
            let (ok, message, clear) = if status == AgentStatus::Blocked {
                match backend.insert_text(&key.id, &text).await {
                    Ok(()) => (
                        true,
                        "入力欄に入れました。内容を確認して Enter で確定してください".to_string(),
                        true,
                    ),
                    Err(e) => (false, format!("入力できませんでした: {e}"), false),
                }
            } else {
                match backend.prompt(&key.id, &text).await {
                    Ok(()) => (true, "送信しました".to_string(), true),
                    Err(e) if e.code() == Some("agent_blocked") => (
                        false,
                        "エージェントが入力待ちのため送れませんでした。先に画面の質問に答えてください".to_string(),
                        false,
                    ),
                    Err(crate::backend::BackendError::Transport(crate::transport::TransportError::Timeout(_))) => (
                        false,
                        "応答がなく、届いたか分かりません。画面を確認してください（再送はしません）".to_string(),
                        false,
                    ),
                    Err(e) => (false, format!("送信できませんでした: {e}"), false),
                }
            };
            let _ = tx.send(AppMsg::Action {
                key,
                ok,
                message,
                clear_draft: clear,
            });
        });
    }

    fn send_keys(&mut self, key: &AgentKey, keys: Vec<String>) {
        let Some(backend) = self.backend(key).filter(|_| self.is_live(key)) else {
            self.toast("ホストに接続されていません");
            return;
        };
        let tx = self.tx.clone();
        let key = key.clone();
        tokio::spawn(async move {
            let label = keys.join(" ");
            let r = backend.send_keys(&key.id, &keys).await;
            let (ok, message) = match r {
                Ok(()) => (true, format!("キーを送りました: {label}")),
                Err(e) => (false, format!("キーを送れませんでした: {e}")),
            };
            let _ = tx.send(AppMsg::Action {
                key,
                ok,
                message,
                clear_draft: false,
            });
        });
    }

    fn request_attach(&mut self, key: &AgentKey) {
        let Some(h) = self.host(&key.host) else {
            return;
        };
        let Some(backend) = h.handle.backend().filter(|_| h.state.is_connected()) else {
            self.toast("ホストに接続されていません");
            return;
        };
        let argv = h
            .handle
            .transport()
            .interactive_argv(&backend.attach_argv(&key.id));
        self.attach = Some(argv);
    }

    // ------------------------------------------------------------------ hosts

    fn key_hosts(&mut self, k: KeyEvent) {
        if self.hosts_view.add.is_some() {
            self.key_add(k);
            return;
        }
        let n = self.hosts.len();
        if self.hosts_view.confirm_remove {
            if k.code == KeyCode::Char('y') {
                self.remove_selected_host();
            }
            self.hosts_view.confirm_remove = false;
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.view = View::Console,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.hosts_view.selected = (self.hosts_view.selected + 1).min(n - 1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.hosts_view.selected = self.hosts_view.selected.saturating_sub(1)
            }
            KeyCode::Char('r') | KeyCode::Enter => {
                if let Some(h) = self.hosts.get(self.hosts_view.selected) {
                    h.handle.reconnect();
                    self.toast(format!("{} に再接続します", h.cfg.name));
                }
            }
            KeyCode::Char('a') => self.start_add(),
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(h) = self.hosts.get(self.hosts_view.selected) {
                    if self.cfg.hosts.iter().any(|c| c.name == h.cfg.name) {
                        self.hosts_view.confirm_remove = true;
                    } else {
                        self.toast("自動検出されたローカルのホストは削除できません");
                    }
                }
            }
            KeyCode::Char('?') => self.show_help(),
            _ => {}
        }
    }

    fn remove_selected_host(&mut self) {
        let Some(h) = self.hosts.get(self.hosts_view.selected) else {
            return;
        };
        let name = h.cfg.name.clone();
        let mut cfg = self.cfg.clone();
        cfg.hosts.retain(|c| c.name != name);
        if let Err(e) = cfg.save(&self.cfg_path) {
            self.toast(format!("設定を保存できませんでした: {e}"));
            return;
        }
        self.cfg = cfg;
        let entry = self.hosts.remove(self.hosts_view.selected);
        let handle = entry.handle.clone();
        tokio::spawn(async move { handle.shutdown().await });
        self.hosts_view.selected = self
            .hosts_view
            .selected
            .min(self.hosts.len().saturating_sub(1));
        if self.console.host_filter.as_deref() == Some(name.as_str()) {
            self.console.host_filter = None;
        }
        self.toast(format!("{name} を削除しました"));
    }

    fn start_add(&mut self) {
        self.hosts_view.add = Some(AddStep::Pick {
            selected: 0,
            loading: true,
        });
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let c = discover::candidates().await;
            let _ = tx.send(AppMsg::Candidates(c));
        });
    }

    fn key_add(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(step) = self.hosts_view.add.as_mut() else {
            return;
        };
        match step {
            AddStep::Pick { selected, .. } => {
                // Row 0 is "enter manually".
                let n = self.hosts_view.candidates.len() + 1;
                match k.code {
                    KeyCode::Esc => self.hosts_view.add = None,
                    KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1).min(n - 1),
                    KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                    KeyCode::Enter => {
                        if *selected == 0 {
                            self.hosts_view.add = Some(AddStep::Manual {
                                input: TextInput::default(),
                            });
                        } else if let Some(c) =
                            self.hosts_view.candidates.get(*selected - 1).cloned()
                        {
                            self.probe_target(c.target);
                        }
                    }
                    _ => {}
                }
            }
            AddStep::Manual { input } => match k.code {
                KeyCode::Esc => self.hosts_view.add = None,
                KeyCode::Enter => {
                    let t = input.text().trim().to_string();
                    if !t.is_empty() {
                        self.probe_target(t);
                    }
                }
                KeyCode::Backspace => input.backspace(),
                KeyCode::Left => input.left(),
                KeyCode::Right => input.right(),
                KeyCode::Char('u') if ctrl => input.kill_to_start(),
                KeyCode::Char(c) if !ctrl => input.insert(c),
                _ => {}
            },
            AddStep::Probing { .. } => {
                if k.code == KeyCode::Esc {
                    self.hosts_view.add = None;
                }
            }
            AddStep::Choose {
                name,
                options,
                selected,
                ..
            } => match k.code {
                KeyCode::Esc => self.hosts_view.add = None,
                KeyCode::Down => *selected = (*selected + 1).min(options.len().saturating_sub(1)),
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Char(' ') if !options.is_empty() => {
                    let o = &mut options[*selected];
                    o.2 = !o.2;
                }
                KeyCode::Backspace => name.backspace(),
                KeyCode::Left => name.left(),
                KeyCode::Right => name.right(),
                KeyCode::Char(c) if !ctrl && c != ' ' => name.insert(c),
                KeyCode::Enter => self.finish_add(),
                _ => {}
            },
        }
    }

    fn probe_target(&mut self, target: String) {
        self.hosts_view.add = Some(AddStep::Probing {
            target: target.clone(),
        });
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let t = SshTransport::new(format!("probe-{}", sanitize(&target)), vec![target.clone()]);
            let result = discover::probe(&t).await.map_err(|e| e.to_string());
            t.shutdown().await;
            let _ = tx.send(AppMsg::Probed { target, result });
        });
    }

    fn on_probed(&mut self, target: String, result: Result<ProbeResult, String>) {
        let still =
            matches!(&self.hosts_view.add, Some(AddStep::Probing { target: t }) if *t == target);
        if !still {
            return;
        }
        match result {
            Err(e) => {
                self.toast(format!("{target} に接続できませんでした: {e}"));
                self.hosts_view.add = Some(AddStep::Manual {
                    input: {
                        let mut t = TextInput::default();
                        t.set(&target);
                        t
                    },
                });
            }
            Ok(p) => {
                let options = host_options(&target, &p);
                if options.is_empty() {
                    let why = if p.herdr_bin.is_none() && p.core_bin.is_none() {
                        "herdr が見つかりません"
                    } else {
                        "動いている herdr のセッションが見つかりません"
                    };
                    self.toast(format!("{target}: {why}"));
                    self.hosts_view.add = None;
                    return;
                }
                let mut name = TextInput::default();
                name.set(&self.unique_name(&short_host(&target)));
                self.hosts_view.add = Some(AddStep::Choose {
                    target,
                    name,
                    options,
                    selected: 0,
                });
            }
        }
    }

    fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| {
            self.hosts.iter().any(|h| h.cfg.name == n) || self.cfg.hosts.iter().any(|h| h.name == n)
        };
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base}-{i}"))
            .find(|n| !taken(n))
            .expect("infinite")
    }

    fn finish_add(&mut self) {
        let Some(AddStep::Choose { name, options, .. }) = self.hosts_view.add.take() else {
            return;
        };
        let base = name.text().trim().to_string();
        if base.is_empty() {
            self.toast("名前を入力してください");
            return;
        }
        let chosen: Vec<HostConfig> = options.into_iter().filter(|o| o.2).map(|o| o.1).collect();
        if chosen.is_empty() {
            self.toast("追加するサーバーを選んでください (Space)");
            return;
        }
        let mut cfg = self.cfg.clone();
        let mut added = Vec::new();
        for (i, mut h) in chosen.into_iter().enumerate() {
            let suffix = match (h.kind, h.session.as_deref()) {
                (ServerKind::HerdrCore, _) => "-core".to_string(),
                (_, Some(s)) if s != "default" => format!(":{s}"),
                _ if i == 0 => String::new(),
                _ => format!("-{i}"),
            };
            h.name = self.unique_name(&format!("{base}{suffix}"));
            if cfg.hosts.iter().any(|c| c.name == h.name) {
                continue;
            }
            cfg.hosts.push(h.clone());
            added.push(h);
        }
        if let Err(e) = cfg.save(&self.cfg_path) {
            self.toast(format!("設定を保存できませんでした: {e}"));
            return;
        }
        self.cfg = cfg;
        let names: Vec<String> = added.iter().map(|h| h.name.clone()).collect();
        for h in added {
            self.add_host_session(h);
        }
        self.toast(format!("追加しました: {}", names.join(", ")));
    }

    // ------------------------------------------------------------------ usage

    fn open_usage(&mut self) {
        self.view = View::Usage;
        self.refresh_usage();
    }

    fn refresh_usage(&mut self) {
        // Local machine once, plus each remote machine once.
        let mut seen_local = false;
        let mut seen_routes: Vec<Vec<String>> = Vec::new();
        let mut targets: Vec<(HostId, Arc<dyn Transport>, bool)> = Vec::new();
        for h in &self.hosts {
            if h.cfg.is_local() || h.handle.transport().is_local() {
                if seen_local {
                    continue;
                }
                seen_local = true;
                targets.push(("local".into(), h.handle.transport(), true));
            } else if !seen_routes.contains(&h.cfg.routes) {
                seen_routes.push(h.cfg.routes.clone());
                targets.push((h.cfg.name.clone(), h.handle.transport(), false));
            }
        }
        if !seen_local {
            targets.insert(
                0,
                (
                    "local".into(),
                    Arc::new(crate::transport::LocalTransport),
                    true,
                ),
            );
        }
        for (host, transport, local) in targets {
            if !self.usage.loading.insert(host.clone()) {
                continue;
            }
            let tx = self.tx.clone();
            let offset = self.utc_offset;
            tokio::spawn(async move {
                let result = if local {
                    let home = std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default();
                    tokio::task::spawn_blocking(move || crate::usage::scan(&home, 30, offset))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r.map_err(|e| e.to_string()))
                } else {
                    remote_usage(transport.as_ref()).await
                };
                let _ = tx.send(AppMsg::Usage { host, result });
            });
        }
    }

    fn key_usage(&mut self, k: KeyEvent) {
        let names: Vec<HostId> = self.usage.reports.keys().cloned().collect();
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => self.view = View::Console,
            KeyCode::Char('r') => self.refresh_usage(),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.usage.host = match &self.usage.host {
                    None => names.first().cloned(),
                    Some(cur) => names
                        .iter()
                        .position(|n| n == cur)
                        .and_then(|i| names.get(i + 1).cloned()),
                };
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.usage.host = match &self.usage.host {
                    None => names.last().cloned(),
                    Some(cur) => names
                        .iter()
                        .position(|n| n == cur)
                        .and_then(|i| i.checked_sub(1))
                        .and_then(|i| names.get(i).cloned()),
                };
            }
            KeyCode::Char('?') => self.show_help(),
            _ => {}
        }
    }

    /// Stop every session (on quit).
    pub async fn shutdown(&mut self) {
        self.detail = None;
        for h in &self.hosts {
            h.handle.shutdown().await;
        }
    }
}

/// User-facing text for a Chat problem.
pub fn chat_error_text(e: &transcript::TranscriptError) -> String {
    use transcript::TranscriptError as E;
    match e {
        E::UnsupportedAgent => {
            "このエージェントのチャット表示には対応していません（Claude Code と Codex のみ）".into()
        }
        E::NoSession => "herdr がこのエージェントのセッションを報告していません。ホストで `herdr integration install claude`（Codex なら codex）を実行し、エージェントを起動し直してください".into(),
        E::NotFound => {
            "会話の記録ファイルがまだ見つかりません（最初のメッセージのあとに作られます）".into()
        }
        other => other.to_string(),
    }
}

fn scroll(d: &mut DetailState, delta: i64) {
    let target = match d.mode {
        DetailMode::Peek => &mut d.scroll,
        DetailMode::Chat => &mut d.chat_scroll,
    };
    *target = (*target as i64 + delta).max(0) as usize;
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// `user@box.tail123.ts.net` -> `box`.
pub fn short_host(target: &str) -> String {
    let t = target.strip_prefix("ssh://").unwrap_or(target);
    let t = t.rsplit('@').next().unwrap_or(t);
    let t = t.split(':').next().unwrap_or(t);
    let first = t.split('.').next().unwrap_or(t);
    if first.is_empty() {
        t.to_string()
    } else {
        first.to_string()
    }
}

/// The servers a probe found, as host configs to choose from.
pub fn host_options(target: &str, p: &ProbeResult) -> Vec<(String, HostConfig, bool)> {
    let mut out = Vec::new();
    for s in &p.herdr_sockets {
        out.push((
            format!("Herdr TUI — セッション {} ({})", s.session, s.path),
            HostConfig {
                name: String::new(),
                kind: ServerKind::HerdrTui,
                session: (s.session != "default").then(|| s.session.clone()),
                routes: vec![target.to_string()],
                socket: None,
                bin: None,
                enabled: true,
            },
            s.session == "default",
        ));
    }
    if p.core_bin.is_some() && p.core_socket.is_some() {
        out.push((
            "herdr-core".to_string(),
            HostConfig {
                name: String::new(),
                kind: ServerKind::HerdrCore,
                session: None,
                routes: vec![target.to_string()],
                socket: None,
                bin: None,
                enabled: true,
            },
            p.herdr_sockets.is_empty(),
        ));
    }
    if !out.iter().any(|o| o.2)
        && let Some(o) = out.first_mut()
    {
        o.2 = true;
    }
    out
}

/// Run `koolie usage --json` on a remote host.
pub async fn remote_usage(t: &dyn Transport) -> Result<UsageReport, String> {
    const SCRIPT: &str = r#"for k in koolie "$HOME/.cargo/bin/koolie" "$HOME/.local/bin/koolie"; do
  if command -v "$k" >/dev/null 2>&1; then exec "$k" usage --json --days "$1"; fi
done
echo "koolie-not-found" >&2; exit 127"#;
    let out = t
        .exec(
            &[
                "sh".into(),
                "-c".into(),
                SCRIPT.into(),
                "sh".into(),
                "30".into(),
            ],
            None,
            Duration::from_secs(120),
        )
        .await
        .map_err(|e| e.to_string())?;
    if out.code == Some(127) && out.stderr_str().contains("koolie-not-found") {
        return Err("このホストに koolie を入れると使用量を表示できます".into());
    }
    if !out.success() {
        return Err(format!(
            "koolie usage が失敗しました: {}",
            out.stderr_str().trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("使用量を読めませんでした: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::HerdrSocket;

    #[test]
    fn short_host_names() {
        assert_eq!(short_host("studio.tail1234.ts.net"), "studio");
        assert_eq!(short_host("me@box"), "box");
        assert_eq!(short_host("ssh://me@box.lan:2222"), "box");
        assert_eq!(short_host("10.0.0.5"), "10");
    }

    #[test]
    fn options_from_probe() {
        let p = ProbeResult {
            herdr_sockets: vec![
                HerdrSocket {
                    session: "default".into(),
                    path: "/h/.config/herdr/herdr.sock".into(),
                },
                HerdrSocket {
                    session: "work".into(),
                    path: "/h/.config/herdr/sessions/work/herdr.sock".into(),
                },
            ],
            core_bin: Some("/h/.local/bin/herdr-core".into()),
            core_socket: Some("/run/user/1/herdr-core/default.sock".into()),
            ..Default::default()
        };
        let o = host_options("box", &p);
        assert_eq!(o.len(), 3);
        assert!(o[0].2 && !o[1].2 && !o[2].2);
        assert_eq!(o[1].1.session.as_deref(), Some("work"));
        assert_eq!(o[2].1.kind, ServerKind::HerdrCore);
        let only_core = ProbeResult {
            core_bin: Some("x".into()),
            core_socket: Some("y".into()),
            ..Default::default()
        };
        let o = host_options("box", &only_core);
        assert_eq!(o.len(), 1);
        assert!(o[0].2);
        assert!(host_options("box", &ProbeResult::default()).is_empty());
    }
}
