//! Codex rollouts: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`.
//!
//! Every line is `{timestamp, type, payload}`. Two dialects exist and the
//! first line (`session_meta`) says which one a file uses:
//!
//! - **Paginated** (`history_mode: "paginated"`, current Codex): the
//!   conversation is the `item_completed` events, each naming its `turn_id`;
//!   `task_started`, `task_complete` and `turn_aborted` open and close turns.
//!   `response_item` lines are model input that repeats the items, so they
//!   are not read.
//! - **Legacy** (older Codex): messages are the `user_message` and
//!   `agent_message` events, tool calls are `response_item`s
//!   (`function_call`, `custom_tool_call`, ...). Files without those events
//!   fall back to the `response_item` messages, minus injected context.

use super::util::{
    MAX_SUMMARY_CHARS, Obj, clip_text, command_text, one_line, parse_ts, str_of, summarize_value,
    text_of,
};
use super::{Conversation, Step, Turn};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// In a prompt sent from an IDE, what follows the last of these is the
/// user's text and what precedes it is context.
const IDE_REQUEST_MARKER: &str = "## My request for Codex:";

/// An answer to an asynchronous question, not a message.
const QUESTION_REPLY_PREFIX: &str = "<send_user_message_question_reply>";

/// User-role text that Codex wrote for the model.
const INJECTED_PREFIXES: &[&str] = &[
    "<environment_context>",
    "<user_instructions>",
    "<codex_internal_context",
    "<recommended_plugins>",
    "<turn_aborted>",
    "<skill>",
    "<hook_prompt",
    "<subagent_notification>",
    "<user_shell_command>",
    "<user_action>",
    "<INSTRUCTIONS>",
    "<system-reminder>",
    "<permissions instructions>",
    "<skills_instructions>",
    "<collaboration_mode>",
    "<apps_instructions>",
    "<multi_agent_mode>",
    "# AGENTS.md instructions",
];

/// Item ids remembered to skip a repeated item.
const SEEN_ITEMS_KEPT: usize = 8192;

/// What the parser remembers about a turn beyond what the UI sees.
#[derive(Debug, Default)]
struct TurnState {
    /// A terminal event closed the turn. Items recorded later still join it,
    /// without reopening it or displacing its answer.
    completed: bool,
    /// Legacy: a `task_started` belongs to this turn.
    has_task: bool,
    /// Legacy: the prompt came from a `response_item`; the `user_message`
    /// event that follows is the same prompt.
    prompt_from_response: bool,
    /// Legacy: likewise for the answer and the `agent_message` event.
    answer_from_response: bool,
    last_ts: Option<i64>,
}

#[derive(Debug, Default)]
pub(super) struct CodexParser {
    conv: Conversation,
    /// One entry per turn of `conv`.
    states: Vec<TurnState>,
    meta_seen: bool,
    paginated: bool,
    last_ordinal: Option<u64>,

    // Paginated dialect.
    /// `turn_id` to the turn that takes its items.
    turn_index: HashMap<String, usize>,
    /// Turns started but without anything to show yet, and their start time.
    pending_start: HashMap<String, Option<i64>>,
    /// A reviewer's turn to the review turn it works for.
    aliases: HashMap<String, String>,
    /// Every `turn_id` a line has named.
    known_turns: HashSet<String>,
    seen_items: HashSet<String>,

    // Legacy dialect.
    has_user_events: bool,
    has_agent_events: bool,
    has_task_events: bool,
    /// A task started and nothing of it is shown yet: its start time.
    pending_task: Option<Option<i64>>,
}

impl CodexParser {
    pub(super) fn conversation(&self) -> &Conversation {
        &self.conv
    }

    pub(super) fn line(&mut self, value: &Value) {
        let Some(obj) = value.as_object() else {
            return;
        };
        // A paginated rollout numbers its lines; a repeated number is a
        // repeated line.
        if let Some(ordinal) = obj.get("ordinal").and_then(Value::as_u64) {
            if self.last_ordinal.is_some_and(|last| ordinal <= last) {
                return;
            }
            self.last_ordinal = Some(ordinal);
        }
        let Some(payload) = obj.get("payload").and_then(Value::as_object) else {
            return;
        };
        let ts = parse_ts(obj.get("timestamp"));
        match str_of(obj, "type") {
            Some("session_meta") => {
                // Only the first one describes this file.
                if !self.meta_seen {
                    self.meta_seen = true;
                    self.paginated = str_of(payload, "history_mode") == Some("paginated");
                }
            }
            Some("turn_context") => {
                if let Some(model) = text_of(payload, "model")
                    && self.conv.model.as_deref() != Some(model)
                {
                    self.conv.model = Some(model.to_string());
                }
                if self.paginated
                    && let Some(id) = str_of(payload, "turn_id")
                {
                    self.note_turn(id, str_of(payload, "root_turn_id"));
                }
            }
            Some("event_msg") => self.event(payload, ts),
            Some("response_item") if !self.paginated => self.response_item(payload, ts),
            _ => {}
        }
        if let (Some(ts), Some(state)) = (ts, self.states.last_mut()) {
            state.last_ts = Some(ts);
        }
    }

    fn event(&mut self, p: &Obj, ts: Option<i64>) {
        match str_of(p, "type") {
            Some("task_started" | "turn_started") => self.task_started(p, ts),
            Some("task_complete" | "turn_complete") => self.task_ended(p, ts, false),
            Some("turn_aborted") => self.task_ended(p, ts, true),
            Some("item_completed") => self.item_completed(p, ts),
            _ if self.paginated => {}
            Some("user_message") => {
                self.has_user_events = true;
                let images = p
                    .get("images")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty());
                let message = str_of(p, "message").unwrap_or("");
                if is_injected(message.trim_start()) {
                    return;
                }
                let Some(text) = clean_user_text(message, images) else {
                    return;
                };
                // The response item just before this event was the same prompt.
                if let (Some(turn), Some(state)) =
                    (self.conv.turns.last_mut(), self.states.last_mut())
                    && state.prompt_from_response
                    && turn.is_empty()
                {
                    turn.prompt = Some(text);
                    state.prompt_from_response = false;
                    return;
                }
                self.legacy_prompt(text, ts, false);
            }
            Some("agent_message") => {
                self.has_agent_events = true;
                let Some(text) = text_of(p, "message") else {
                    return;
                };
                let text = clip_text(text);
                if let (Some(turn), Some(state)) =
                    (self.conv.turns.last_mut(), self.states.last_mut())
                    && state.answer_from_response
                    && turn.answer.is_some()
                {
                    turn.answer = Some(text);
                    state.answer_from_response = false;
                    return;
                }
                self.legacy_text(text, ts, false);
            }
            Some("agent_reasoning" | "agent_reasoning_raw_content") => {
                self.legacy_step(Step::Thinking, ts);
            }
            Some("thread_rolled_back") => {
                let n = p.get("num_turns").and_then(Value::as_u64).unwrap_or(0);
                let keep = self
                    .conv
                    .turns
                    .len()
                    .saturating_sub(usize::try_from(n).unwrap_or(usize::MAX));
                self.conv.turns.truncate(keep);
                self.states.truncate(keep);
                self.pending_task = None;
                if let Some(turn) = self.conv.turns.last_mut() {
                    turn.in_progress = false;
                }
            }
            _ => {}
        }
    }

    /// Remember that a line named turn `id`. A turn started under a root
    /// turn this file also names is a reviewer working for that turn.
    fn note_turn(&mut self, id: &str, root: Option<&str>) {
        if let Some(root) = root
            && root != id
            && self.known_turns.contains(root)
            && !self.turn_index.contains_key(id)
        {
            self.aliases.insert(id.to_string(), root.to_string());
        }
        if !self.known_turns.contains(id) {
            self.known_turns.insert(id.to_string());
        }
    }

    fn task_started(&mut self, p: &Obj, ts: Option<i64>) {
        let started = parse_ts(p.get("started_at")).or(ts);
        if self.paginated {
            let Some(id) = str_of(p, "turn_id") else {
                return;
            };
            self.note_turn(id, str_of(p, "root_turn_id"));
            if !self.aliases.contains_key(id) && !self.turn_index.contains_key(id) {
                self.pending_start.insert(id.to_string(), started);
            }
            return;
        }
        self.has_task_events = true;
        if let (Some(turn), Some(state)) = (self.conv.turns.last_mut(), self.states.last_mut())
            && !state.has_task
            && !state.completed
            && turn.prompt.is_some()
            && turn.is_empty()
        {
            // The prompt was recorded before its task started.
            state.has_task = true;
            turn.in_progress = true;
            turn.ended_at = None;
            return;
        }
        self.close_last();
        self.pending_task = Some(started);
    }

    /// `task_complete` or, with `aborted`, `turn_aborted`.
    fn task_ended(&mut self, p: &Obj, ts: Option<i64>, aborted: bool) {
        let ended = parse_ts(p.get("completed_at")).or(ts);
        let idx = if self.paginated {
            match str_of(p, "turn_id") {
                Some(id) => {
                    // A reviewer's turn ending does not end the review.
                    if self.aliases.contains_key(id) {
                        return;
                    }
                    self.pending_start.remove(id);
                    self.turn_index.get(id).copied()
                }
                None => self.conv.turns.len().checked_sub(1),
            }
        } else {
            self.has_task_events = true;
            // A task that showed nothing has no turn to end.
            if self.pending_task.take().is_some() {
                None
            } else {
                self.conv.turns.len().checked_sub(1)
            }
        };
        let Some(idx) = idx else {
            return;
        };
        let (Some(turn), Some(state)) = (self.conv.turns.get_mut(idx), self.states.get_mut(idx))
        else {
            return;
        };
        turn.in_progress = false;
        turn.ended_at = ended.or(turn.ended_at);
        state.completed = true;
        if aborted {
            turn.interrupted = matches!(str_of(p, "reason"), None | Some("interrupted"));
            return;
        }
        if let Some(message) = p
            .get("error")
            .and_then(Value::as_object)
            .and_then(|e| text_of(e, "message"))
        {
            turn.push_text(clip_text(&format!("Error: {message}")));
        }
        // A turn that ended on commentary: that text is its answer.
        if turn.answer.is_none()
            && let Some(Step::Text(_)) = turn.steps.last()
            && let Some(Step::Text(text)) = turn.steps.pop()
        {
            turn.answer = Some(text);
        }
    }

    fn item_completed(&mut self, p: &Obj, ts: Option<i64>) {
        let Some(item) = p.get("item").and_then(Value::as_object) else {
            return;
        };
        let Some(kind) = str_of(item, "type") else {
            return;
        };
        if !self.paginated {
            // A legacy rollout writes items only for a few activities, never
            // for messages or commands: seeing those means the header was
            // missed and this is a paginated file.
            if matches!(
                kind,
                "UserMessage" | "AgentMessage" | "Reasoning" | "CommandExecution" | "FileChange"
            ) {
                self.paginated = true;
            } else {
                if kind == "Plan"
                    && let Some(text) = text_of(item, "text")
                {
                    self.legacy_step(Step::Text(clip_text(text)), ts);
                }
                return;
            }
        }
        let raw_turn = str_of(p, "turn_id").unwrap_or("");
        self.note_turn(raw_turn, None);
        let reviewer = self.aliases.contains_key(raw_turn);
        let turn_id = self.resolve(raw_turn);
        if let Some(item_id) = str_of(item, "id") {
            if self.seen_items.len() >= SEEN_ITEMS_KEPT {
                self.seen_items.clear();
            }
            if !self.seen_items.insert(format!("{turn_id}/{item_id}")) {
                return;
            }
        }
        match kind {
            "UserMessage" => {
                // The reviewer's prompt is one Codex wrote for it.
                if reviewer {
                    return;
                }
                if let Some(text) = user_item_text(item) {
                    self.user_item(&turn_id, text, ts);
                }
            }
            "AgentMessage" => {
                let Some(text) = agent_item_text(item) else {
                    return;
                };
                let idx = self.ensure_turn(&turn_id, ts);
                let commentary = str_of(item, "phase") == Some("commentary");
                if commentary {
                    self.add_step(idx, Step::Text(text));
                } else if let Some(turn) = self.conv.turns.get_mut(idx) {
                    turn.push_text(text);
                }
            }
            "Reasoning" => {
                let idx = self.ensure_turn(&turn_id, ts);
                self.add_step(idx, Step::Thinking);
            }
            "Plan" => {
                if let Some(text) = text_of(item, "text") {
                    let idx = self.ensure_turn(&turn_id, ts);
                    self.add_step(idx, Step::Text(clip_text(text)));
                }
            }
            "EnteredReviewMode" => {
                let idx = self.ensure_turn(&turn_id, ts);
                if let Some(turn) = self.conv.turns.get_mut(idx)
                    && turn.prompt.is_none()
                    && turn.is_empty()
                {
                    let hint = str_of(item, "user_facing_hint").unwrap_or("");
                    turn.prompt = Some(clip_text(&format!("/review {hint}")));
                }
            }
            _ => {
                if let Some((name, summary)) = item_tool(kind, item) {
                    let idx = self.ensure_turn(&turn_id, ts);
                    self.add_step(idx, Step::Tool { name, summary });
                }
            }
        }
    }

    fn resolve(&self, turn_id: &str) -> String {
        let mut id = turn_id;
        for _ in 0..8 {
            match self.aliases.get(id) {
                Some(root) => id = root,
                None => break,
            }
        }
        id.to_string()
    }

    /// A paginated user message: the prompt of its turn, or, sent while the
    /// turn already ran, the start of a new one under the same id.
    fn user_item(&mut self, turn_id: &str, text: String, ts: Option<i64>) {
        let existing = self.turn_index.get(turn_id).copied();
        if let Some(idx) = existing
            && let Some(turn) = self.conv.turns.get_mut(idx)
        {
            if turn.prompt.is_none() && turn.is_empty() {
                turn.prompt = Some(text);
                return;
            }
            let completed = self.states.get(idx).is_some_and(|s| s.completed);
            let new = self.materialize(turn_id, Some(text), ts);
            if completed
                && let (Some(turn), Some(state)) =
                    (self.conv.turns.get_mut(new), self.states.get_mut(new))
            {
                turn.in_progress = false;
                state.completed = true;
            }
            return;
        }
        self.materialize(turn_id, Some(text), ts);
    }

    fn ensure_turn(&mut self, turn_id: &str, ts: Option<i64>) -> usize {
        match self.turn_index.get(turn_id) {
            Some(&idx) => idx,
            None => self.materialize(turn_id, None, ts),
        }
    }

    /// Append the turn that takes the items of `turn_id` from now on.
    fn materialize(&mut self, turn_id: &str, prompt: Option<String>, ts: Option<i64>) -> usize {
        self.close_last();
        let started = self.pending_start.remove(turn_id).flatten().or(ts);
        self.conv.turns.push(Turn {
            prompt,
            in_progress: true,
            started_at: started,
            ..Turn::default()
        });
        self.states.push(TurnState::default());
        let idx = self.conv.turns.len() - 1;
        self.turn_index.insert(turn_id.to_string(), idx);
        idx
    }

    /// Only the newest turn can be running.
    fn close_last(&mut self) {
        if let (Some(turn), Some(state)) = (self.conv.turns.last_mut(), self.states.last())
            && turn.in_progress
        {
            turn.in_progress = false;
            turn.ended_at = turn.ended_at.or(state.last_ts);
        }
    }

    fn add_step(&mut self, idx: usize, step: Step) {
        let (Some(turn), Some(state)) = (self.conv.turns.get_mut(idx), self.states.get_mut(idx))
        else {
            return;
        };
        state.answer_from_response = false;
        if state.completed && turn.answer.is_some() {
            // Recorded after the turn ended (a command left running in the
            // background): it belongs above the answer.
            if step != Step::Thinking || turn.steps.last() != Some(&Step::Thinking) {
                turn.steps.push(step);
            }
        } else {
            turn.push_step(step);
        }
    }

    // ---- Legacy dialect ----

    fn response_item(&mut self, p: &Obj, ts: Option<i64>) {
        let Some(kind) = str_of(p, "type") else {
            return;
        };
        match kind {
            "message" => match str_of(p, "role") {
                Some("user") if !self.has_user_events => {
                    let (text, images) = message_text(p, "input_text", true);
                    if let Some(text) = clean_user_text(&text, images) {
                        self.legacy_prompt(text, ts, true);
                    }
                }
                Some("assistant") if !self.has_agent_events => {
                    let (text, _) = message_text(p, "output_text", false);
                    if !text.trim().is_empty() {
                        self.legacy_text(clip_text(&text), ts, true);
                    }
                }
                _ => {}
            },
            "reasoning" => self.legacy_step(Step::Thinking, ts),
            "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                // The agent goes on with the result.
                if !self.has_task_events
                    && let (Some(turn), Some(state)) =
                        (self.conv.turns.last_mut(), self.states.last())
                    && !state.completed
                {
                    turn.in_progress = true;
                    turn.ended_at = None;
                }
            }
            _ => {
                if let Some((name, summary)) = response_tool(kind, p) {
                    self.legacy_step(Step::Tool { name, summary }, ts);
                }
            }
        }
    }

    fn legacy_prompt(&mut self, text: String, ts: Option<i64>, from_response: bool) {
        self.close_last();
        let (started, has_task) = match self.pending_task.take() {
            Some(started) => (started.or(ts), true),
            None => (ts, false),
        };
        self.conv.turns.push(Turn {
            prompt: Some(text),
            in_progress: true,
            started_at: started,
            ..Turn::default()
        });
        self.states.push(TurnState {
            has_task,
            prompt_from_response: from_response,
            ..TurnState::default()
        });
    }

    /// The turn that takes agent activity: the newest, or a new one without
    /// a prompt when a task started since (or nothing exists yet).
    fn legacy_turn(&mut self, ts: Option<i64>) -> usize {
        if self.pending_task.is_some() || self.conv.turns.is_empty() {
            self.close_last();
            let pending = self.pending_task.take();
            self.conv.turns.push(Turn {
                in_progress: true,
                started_at: pending.flatten().or(ts),
                ..Turn::default()
            });
            self.states.push(TurnState {
                has_task: pending.is_some(),
                ..TurnState::default()
            });
        }
        self.conv.turns.len() - 1
    }

    fn legacy_text(&mut self, text: String, ts: Option<i64>, from_response: bool) {
        let idx = self.legacy_turn(ts);
        let (Some(turn), Some(state)) = (self.conv.turns.get_mut(idx), self.states.get_mut(idx))
        else {
            return;
        };
        turn.push_text(text);
        state.answer_from_response = from_response;
        // Without task events nothing says a turn ended: text with nothing
        // after it reads as the end until more arrives.
        if !self.has_task_events {
            turn.in_progress = false;
            turn.ended_at = ts;
        }
    }

    fn legacy_step(&mut self, step: Step, ts: Option<i64>) {
        let idx = self.legacy_turn(ts);
        self.add_step(idx, step);
        if !self.has_task_events
            && let (Some(turn), Some(state)) = (self.conv.turns.get_mut(idx), self.states.get(idx))
            && !state.completed
        {
            turn.in_progress = true;
            turn.ended_at = None;
        }
    }
}

fn is_injected(trimmed: &str) -> bool {
    INJECTED_PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

/// Text that is exactly one `<name>…</name>` element: context Codex wrapped
/// for the model, whatever its name.
fn is_wrapped_context(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix('<') else {
        return false;
    };
    let Some(end) = rest.find('>') else {
        return false;
    };
    let name = &rest[..end];
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && trimmed.ends_with(&format!("</{name}>"))
}

/// What the user typed, from the text Codex recorded: without an IDE's
/// context preamble, and `None` for an answer to a question or nothing at
/// all. `images` says the message carried attachments.
fn clean_user_text(text: &str, images: bool) -> Option<String> {
    let mut t = text.trim();
    if let Some(pos) = t.rfind(IDE_REQUEST_MARKER) {
        t = t[pos + IDE_REQUEST_MARKER.len()..].trim();
    }
    if t.starts_with(QUESTION_REPLY_PREFIX) {
        return None;
    }
    if t.is_empty() {
        return images.then(|| "[image]".to_string());
    }
    Some(clip_text(t))
}

/// The text parts of a `response_item` message, and whether it has images.
/// With `skip_injected`, parts Codex wrote for the model are left out.
fn message_text(p: &Obj, part_type: &str, skip_injected: bool) -> (String, bool) {
    let mut text = String::new();
    let mut images = false;
    match p.get("content") {
        Some(Value::String(s)) => text.push_str(s),
        Some(Value::Array(parts)) => {
            for part in parts.iter().filter_map(Value::as_object) {
                let kind = str_of(part, "type");
                if kind == Some("input_image") {
                    images = true;
                }
                if kind != Some(part_type) && kind != Some("text") {
                    continue;
                }
                let Some(piece) = text_of(part, "text") else {
                    continue;
                };
                let trimmed = piece.trim();
                if skip_injected && (is_injected(trimmed) || is_wrapped_context(trimmed)) {
                    continue;
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(piece);
            }
        }
        _ => {}
    }
    (text, images)
}

/// The prompt of a paginated `UserMessage` item.
fn user_item_text(item: &Obj) -> Option<String> {
    let mut text = String::new();
    let mut images = false;
    let mut skills: Vec<&str> = Vec::new();
    match item.get("content") {
        Some(Value::String(s)) => text.push_str(s),
        Some(Value::Array(parts)) => {
            for part in parts.iter().filter_map(Value::as_object) {
                match str_of(part, "type") {
                    // The TUI joins text parts with nothing between them.
                    Some("text") => text.push_str(str_of(part, "text").unwrap_or("")),
                    Some("local_image" | "image") => images = true,
                    Some("skill") => skills.extend(str_of(part, "name")),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let cleaned = clean_user_text(&text, images)?;
    // `$name args` runs a skill: show it as the `/name` every agent uses.
    if let Some(rest) = cleaned.strip_prefix('$') {
        let name = rest.split_whitespace().next().unwrap_or("");
        if skills.contains(&name) {
            return Some(format!("/{rest}"));
        }
    }
    Some(cleaned)
}

/// The text of a paginated `AgentMessage` item, with the questions it asks.
fn agent_item_text(item: &Obj) -> Option<String> {
    let mut text = String::new();
    match item.get("content") {
        Some(Value::String(s)) => text.push_str(s),
        Some(Value::Array(parts)) => {
            for part in parts.iter().filter_map(Value::as_object) {
                text.push_str(str_of(part, "text").unwrap_or(""));
            }
        }
        _ => {}
    }
    if let Some(questions) = item.get("questions").and_then(Value::as_array) {
        for title in questions
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|q| text_of(q, "title"))
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("? ");
            text.push_str(title.trim());
        }
    }
    if text.trim().is_empty() {
        None
    } else {
        Some(clip_text(&text))
    }
}

fn line(text: &str) -> String {
    one_line(text, MAX_SUMMARY_CHARS)
}

/// Name and summary of a paginated turn item that is a tool call.
fn item_tool(kind: &str, item: &Obj) -> Option<(String, String)> {
    let field = |key: &str| text_of(item, key);
    let tool = |name: &str, summary: String| Some((name.to_string(), summary));
    match kind {
        "CommandExecution" => tool(
            "shell",
            line(
                &item
                    .get("command")
                    .and_then(command_text)
                    .unwrap_or_default(),
            ),
        ),
        "FileChange" => {
            let paths: Vec<&str> = item
                .get("changes")
                .and_then(Value::as_object)
                .map(|changes| changes.keys().map(String::as_str).collect())
                .unwrap_or_default();
            tool("patch", paths_summary(&paths))
        }
        "McpToolCall" => {
            let name = match (field("server"), field("tool")) {
                (Some(server), Some(tool)) => format!("{server}.{tool}"),
                (None, Some(tool)) => tool.to_string(),
                (server, None) => server.unwrap_or("mcp").to_string(),
            };
            Some((
                name,
                summarize_value(item.get("arguments").unwrap_or(&Value::Null)),
            ))
        }
        "DynamicToolCall" => Some((
            field("tool").unwrap_or("tool").to_string(),
            summarize_value(item.get("arguments").unwrap_or(&Value::Null)),
        )),
        "WebSearch" => tool("web_search", line(field("query").unwrap_or(""))),
        "Extension" => match field("kind") {
            Some("web.search") => {
                let query = field("query").or_else(|| {
                    item.get("action")
                        .and_then(Value::as_object)
                        .and_then(|a| a.get("queries"))
                        .and_then(Value::as_array)
                        .and_then(|q| q.first())
                        .and_then(Value::as_str)
                });
                tool("web_search", line(query.unwrap_or("")))
            }
            Some("clock.sleep") => {
                let ms = item.get("durationMs").and_then(Value::as_u64).unwrap_or(0);
                tool("sleep", format!("{}s", ms / 1000))
            }
            Some("image_gen.generation") => {
                tool("image_gen", line(field("revisedPrompt").unwrap_or("")))
            }
            Some(other) => tool(other, String::new()),
            None => None,
        },
        "ImageView" => {
            let path = field("path").unwrap_or("");
            tool(
                "view_image",
                line(path.strip_prefix("file://").unwrap_or(path)),
            )
        }
        "ImageGeneration" => tool("image_gen", line(field("revised_prompt").unwrap_or(""))),
        "CollabAgentToolCall" => Some((
            field("tool").unwrap_or("collab").to_string(),
            line(field("prompt").or_else(|| field("status")).unwrap_or("")),
        )),
        "SubAgentActivity" => tool(
            "subagent",
            line(&format!(
                "{} {}",
                field("kind").unwrap_or(""),
                field("agent_path").unwrap_or("")
            )),
        ),
        _ => None,
    }
}

/// Name and summary of a legacy `response_item` that is a tool call.
fn response_tool(kind: &str, p: &Obj) -> Option<(String, String)> {
    match kind {
        "function_call" => {
            let name = text_of(p, "name")?;
            let summary = match p.get("arguments") {
                // Arguments are JSON in a string.
                Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
                    Ok(args) => function_summary(name, &args),
                    Err(_) => line(raw),
                },
                Some(args) => function_summary(name, args),
                None => String::new(),
            };
            Some((name.to_string(), summary))
        }
        "custom_tool_call" => {
            let name = text_of(p, "name")?;
            let input = str_of(p, "input").unwrap_or("");
            let summary = if name == "apply_patch" {
                patch_summary(input)
            } else {
                line(input)
            };
            Some((name.to_string(), summary))
        }
        "local_shell_call" => {
            let command = p
                .get("action")
                .and_then(Value::as_object)
                .and_then(|a| a.get("command"))
                .and_then(command_text)
                .unwrap_or_default();
            Some(("shell".to_string(), line(&command)))
        }
        "web_search_call" => {
            let query = p
                .get("action")
                .and_then(Value::as_object)
                .and_then(|a| text_of(a, "query"))
                .unwrap_or("");
            Some(("web_search".to_string(), line(query)))
        }
        _ => None,
    }
}

fn function_summary(name: &str, args: &Value) -> String {
    let obj = args.as_object();
    match name {
        "apply_patch" => {
            let patch = obj
                .and_then(|o| text_of(o, "input").or_else(|| text_of(o, "patch")))
                .or_else(|| args.as_str());
            match patch {
                Some(patch) => patch_summary(patch),
                None => summarize_value(args),
            }
        }
        "update_plan" => match obj
            .and_then(|o| o.get("plan"))
            .and_then(Value::as_array)
            .map(Vec::len)
        {
            Some(1) => "1 step".to_string(),
            Some(n) => format!("{n} steps"),
            None => summarize_value(args),
        },
        _ => summarize_value(args),
    }
}

/// The files an `apply_patch` input touches.
fn patch_summary(patch: &str) -> String {
    let paths: Vec<&str> = patch
        .lines()
        .filter_map(|l| {
            ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                .iter()
                .find_map(|prefix| l.strip_prefix(prefix))
        })
        .map(str::trim)
        .collect();
    if paths.is_empty() {
        line(patch)
    } else {
        paths_summary(&paths)
    }
}

fn paths_summary(paths: &[&str]) -> String {
    match paths {
        [] => String::new(),
        [one] => line(one),
        [first, rest @ ..] => line(&format!("{first} (+{} more)", rest.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Obj {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn cleans_user_text() {
        assert_eq!(clean_user_text("  hi  ", false).as_deref(), Some("hi"));
        assert_eq!(
            clean_user_text(
                "# Context from my IDE setup:\n## Open tabs:\n- a.rs\n\n## My request for Codex:\nfix it\n",
                false
            )
            .as_deref(),
            Some("fix it")
        );
        assert_eq!(
            clean_user_text(
                "<send_user_message_question_reply>[]</send_user_message_question_reply>",
                false
            ),
            None
        );
        assert_eq!(clean_user_text(" ", false), None);
        assert_eq!(clean_user_text(" ", true).as_deref(), Some("[image]"));
    }

    #[test]
    fn spots_injected_context() {
        assert!(is_injected(
            "<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"
        ));
        assert!(is_injected("# AGENTS.md instructions for /w\n\nrules"));
        assert!(!is_injected("please read <environment_context>"));
        assert!(is_wrapped_context("<new_thing>\nx\n</new_thing>"));
        assert!(!is_wrapped_context("<b>bold</b> and more"));
        assert!(!is_wrapped_context("a < b > c"));
    }

    #[test]
    fn user_items_join_parts_and_show_skills_as_commands() {
        let item = obj(json!({"type": "UserMessage", "content": [
            {"type": "text", "text": "look at "},
            {"type": "text", "text": "this"},
            {"type": "local_image", "path": "/tmp/a.png"}
        ]}));
        assert_eq!(user_item_text(&item).as_deref(), Some("look at this"));
        let skill = obj(json!({"type": "UserMessage", "content": [
            {"type": "text", "text": "$deploy to staging"},
            {"type": "skill", "name": "deploy", "path": "/s/deploy"}
        ]}));
        assert_eq!(
            user_item_text(&skill).as_deref(),
            Some("/deploy to staging")
        );
        let dollar = obj(json!({"type": "UserMessage", "content": [
            {"type": "text", "text": "$HOME is where?"}
        ]}));
        assert_eq!(user_item_text(&dollar).as_deref(), Some("$HOME is where?"));
        let image = obj(json!({"type": "UserMessage", "content": [
            {"type": "local_image", "path": "/tmp/a.png"}
        ]}));
        assert_eq!(user_item_text(&image).as_deref(), Some("[image]"));
    }

    #[test]
    fn item_tools() {
        let t = |v: Value| {
            let item = obj(v);
            item_tool(str_of(&item, "type").unwrap(), &item).unwrap()
        };
        assert_eq!(
            t(json!({"type": "CommandExecution", "command": ["/bin/bash", "-lc", "cargo test"]})),
            ("shell".into(), "cargo test".into())
        );
        assert_eq!(
            t(json!({"type": "CommandExecution", "command": ["git", "status"]})),
            ("shell".into(), "git status".into())
        );
        assert_eq!(
            t(json!({"type": "FileChange", "changes": {
                "/w/a.rs": {"type": "add", "content": "x"},
                "/w/b.rs": {"type": "update", "unified_diff": "y"}
            }})),
            ("patch".into(), "/w/a.rs (+1 more)".into())
        );
        assert_eq!(
            t(
                json!({"type": "McpToolCall", "server": "files", "tool": "stat",
                "arguments": {"path": "/w/a.rs"}})
            ),
            ("files.stat".into(), "/w/a.rs".into())
        );
        assert_eq!(
            t(json!({"type": "Extension", "kind": "web.search", "query": "rust serde"})),
            ("web_search".into(), "rust serde".into())
        );
        assert_eq!(
            t(json!({"type": "Extension", "kind": "clock.sleep", "durationMs": 20000})),
            ("sleep".into(), "20s".into())
        );
        assert_eq!(
            t(json!({"type": "ImageView", "path": "file:///w/a.png"})),
            ("view_image".into(), "/w/a.png".into())
        );
        let unknown = obj(json!({"type": "ContextCompaction", "id": "c"}));
        assert_eq!(item_tool("ContextCompaction", &unknown), None);
    }

    #[test]
    fn response_tools() {
        let t = |v: Value| {
            let p = obj(v);
            response_tool(str_of(&p, "type").unwrap(), &p).unwrap()
        };
        assert_eq!(
            t(json!({"type": "function_call", "name": "shell",
                "arguments": "{\"command\":[\"bash\",\"-lc\",\"ls -la\"],\"workdir\":\"/w\"}"})),
            ("shell".into(), "ls -la".into())
        );
        assert_eq!(
            t(json!({"type": "function_call", "name": "exec_command",
                "arguments": "{\"cmd\":\"cargo build\"}"})),
            ("exec_command".into(), "cargo build".into())
        );
        assert_eq!(
            t(json!({"type": "function_call", "name": "update_plan",
                "arguments": "{\"plan\":[{\"step\":\"a\"},{\"step\":\"b\"}]}"})),
            ("update_plan".into(), "2 steps".into())
        );
        assert_eq!(
            t(json!({"type": "function_call", "name": "odd", "arguments": "not json"})),
            ("odd".into(), "not json".into())
        );
        assert_eq!(
            t(json!({"type": "custom_tool_call", "name": "apply_patch",
                "input": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: src/b.rs\n+z\n*** End Patch"})),
            ("apply_patch".into(), "src/a.rs (+1 more)".into())
        );
        assert_eq!(
            t(json!({"type": "local_shell_call", "action": {"type": "exec", "command": ["ls"]}})),
            ("shell".into(), "ls".into())
        );
        assert_eq!(
            t(json!({"type": "web_search_call", "action": {"type": "search", "query": "q"}})),
            ("web_search".into(), "q".into())
        );
    }
}
