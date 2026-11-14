//! Cross-format tests: chunked feeding, locating and following files on this
//! machine and through the remote (exec) path, and a check against every
//! real transcript on this machine (`--ignored`).

use super::files::{locate_in, read_chunk};
use super::*;
use crate::model::{AgentSession, SessionRefKind};
use crate::transport::LocalTransport;

const CLAUDE: &str = concat!(
    r#"{"type":"summary","summary":"テストの修正","leafUuid":"x"}"#,
    "\n",
    r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-11-14T01:00:00Z","message":{"role":"user","content":"テストを直して"}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-11-14T01:00:05Z","requestId":"r1","message":{"id":"m1","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"見てみます"}],"stop_reason":null}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"2026-11-14T01:00:06Z","requestId":"r1","message":{"id":"m1","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test","description":"run tests"}}],"stop_reason":"tool_use"}}"#,
    "\n",
    r#"{"type":"user","uuid":"u2","sessionId":"s1","timestamp":"2026-11-14T01:00:20Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"test result: ok"}]}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"a3","sessionId":"s1","timestamp":"2026-11-14T01:00:30Z","requestId":"r2","message":{"id":"m2","role":"assistant","model":"claude-opus-5-5","content":[{"type":"text","text":"直しました。テストはすべて通っています。"}],"stop_reason":"end_turn"}}"#,
    "\n",
    r#"{"type":"user","uuid":"u3","sessionId":"s1","isSidechain":true,"timestamp":"2026-11-14T01:00:31Z","message":{"role":"user","content":"subagent prompt"}}"#,
    "\n",
    r#"not json at all"#,
    "\n",
    r#"{"type":"user","uuid":"u4","sessionId":"s1","timestamp":"2026-11-14T01:01:00Z","message":{"role":"user","content":"次はドキュメント"}}"#,
    "\n",
);

const CODEX: &str = concat!(
    r#"{"timestamp":"2026-11-14T02:00:00Z","type":"session_meta","payload":{"id":"th-1","cwd":"/tmp/p"}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:01Z","type":"event_msg","payload":{"type":"task_started"}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"テストを追加して"}]}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"テストを追加して"}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:05Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"cargo test\"]}","call_id":"c1"}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:09Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:10Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"追加しました"}]}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:10Z","type":"event_msg","payload":{"type":"agent_message","message":"追加しました"}}"#,
    "\n",
    r#"{"timestamp":"2026-11-14T02:00:11Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"追加しました"}}"#,
    "\n",
);

fn parse_whole(h: Harness, text: &str) -> Conversation {
    let mut p = TranscriptParser::new(h);
    p.feed(text.as_bytes());
    p.conversation().clone()
}

fn parse_chunked(h: Harness, text: &str, chunk: usize) -> Conversation {
    let mut p = TranscriptParser::new(h);
    for c in text.as_bytes().chunks(chunk) {
        p.feed(c);
    }
    p.conversation().clone()
}

#[test]
fn claude_fixture_shape() {
    let c = parse_whole(Harness::Claude, CLAUDE);
    assert_eq!(c.turns.len(), 2, "{c:#?}");
    let t = &c.turns[0];
    assert_eq!(t.prompt.as_deref(), Some("テストを直して"));
    assert!(
        t.steps
            .iter()
            .any(|s| matches!(s, Step::Tool { name, summary } if name == "Bash" && summary.contains("cargo test"))),
        "{t:#?}"
    );
    assert_eq!(
        t.answer.as_deref(),
        Some("直しました。テストはすべて通っています。")
    );
    assert!(!t.in_progress);
    assert_eq!(c.turns[1].prompt.as_deref(), Some("次はドキュメント"));
    assert!(c.turns[1].in_progress);
    // The sidechain prompt never shows up.
    assert!(
        !c.turns
            .iter()
            .any(|t| t.prompt.as_deref() == Some("subagent prompt"))
    );
}

#[test]
fn codex_fixture_shape() {
    let c = parse_whole(Harness::Codex, CODEX);
    assert_eq!(c.turns.len(), 1, "{c:#?}");
    let t = &c.turns[0];
    assert_eq!(t.prompt.as_deref(), Some("テストを追加して"));
    assert!(
        t.steps.iter().any(|s| matches!(s, Step::Tool { .. })),
        "{t:#?}"
    );
    assert_eq!(t.answer.as_deref(), Some("追加しました"));
    assert!(!t.in_progress, "{t:#?}");
}

#[test]
fn chunked_feeding_is_equivalent() {
    for (h, text) in [(Harness::Claude, CLAUDE), (Harness::Codex, CODEX)] {
        let whole = parse_whole(h, text);
        for chunk in [1, 2, 3, 7, 64, 1000] {
            assert_eq!(parse_chunked(h, text, chunk), whole, "{h:?} chunk {chunk}");
        }
    }
}

#[test]
fn partial_line_waits_for_newline() {
    let mut p = TranscriptParser::new(Harness::Claude);
    let line = r#"{"type":"user","uuid":"u1","timestamp":"2026-11-14T01:00:00Z","message":{"role":"user","content":"hi"}}"#;
    p.feed(&line.as_bytes()[..20]);
    assert!(p.conversation().turns.is_empty());
    p.feed(&line.as_bytes()[20..]);
    p.feed(b"\n");
    assert_eq!(p.conversation().turns.len(), 1);
}

fn session(agent: &str, kind: SessionRefKind, value: &str) -> AgentSession {
    AgentSession {
        agent: agent.into(),
        kind,
        value: value.into(),
    }
}

#[tokio::test]
async fn locate_claude_and_codex_both_routes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let claude_file = dir.path().join("projects/-tmp-p/5b1c-uuid-1.jsonl");
    std::fs::create_dir_all(claude_file.parent().unwrap()).unwrap();
    std::fs::write(&claude_file, CLAUDE).unwrap();
    let codex_file = dir
        .path()
        .join("sessions/2026/11/14/rollout-2026-11-14T02-00-00-th-abc-123.jsonl");
    std::fs::create_dir_all(codex_file.parent().unwrap()).unwrap();
    std::fs::write(&codex_file, CODEX).unwrap();
    let t = LocalTransport;
    for use_exec in [false, true] {
        let p = locate_in(
            &t,
            &session("claude", SessionRefKind::Id, "5b1c-uuid-1"),
            use_exec,
            Some(&root),
        )
        .await
        .unwrap();
        assert_eq!(p, claude_file.to_str().unwrap(), "exec={use_exec}");
        let p = locate_in(
            &t,
            &session("codex", SessionRefKind::Id, "th-abc-123"),
            use_exec,
            Some(&root),
        )
        .await
        .unwrap();
        assert_eq!(p, codex_file.to_str().unwrap(), "exec={use_exec}");
        let p = locate_in(
            &t,
            &session(
                "claude",
                SessionRefKind::Path,
                claude_file.to_str().unwrap(),
            ),
            use_exec,
            Some(&root),
        )
        .await
        .unwrap();
        assert_eq!(p, claude_file.to_str().unwrap());
        let e = locate_in(
            &t,
            &session("claude", SessionRefKind::Id, "missing-id"),
            use_exec,
            Some(&root),
        )
        .await
        .unwrap_err();
        assert_eq!(e, TranscriptError::NotFound, "exec={use_exec}");
    }
    let e = locate_in(
        &t,
        &session("cursor", SessionRefKind::Id, "x"),
        false,
        Some(&root),
    )
    .await
    .unwrap_err();
    assert_eq!(e, TranscriptError::UnsupportedAgent);
    // Ids that could escape the directory are refused.
    let e = locate_in(
        &t,
        &session("claude", SessionRefKind::Id, "../../etc/passwd"),
        true,
        Some(&root),
    )
    .await
    .unwrap_err();
    assert_eq!(e, TranscriptError::NotFound);
}

#[tokio::test]
async fn read_chunks_both_routes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, "0123456789").unwrap();
    let p = path.to_str().unwrap();
    let t = LocalTransport;
    for use_exec in [false, true] {
        assert_eq!(
            read_chunk(&t, p, 0, use_exec).await.unwrap(),
            files::Chunk::Data(b"0123456789".to_vec())
        );
        assert_eq!(
            read_chunk(&t, p, 4, use_exec).await.unwrap(),
            files::Chunk::Data(b"456789".to_vec())
        );
        assert_eq!(
            read_chunk(&t, p, 10, use_exec).await.unwrap(),
            files::Chunk::Data(Vec::new())
        );
        assert_eq!(
            read_chunk(&t, p, 50, use_exec).await.unwrap(),
            files::Chunk::Shrunk,
            "exec={use_exec}"
        );
    }
}

#[tokio::test]
async fn follower_appends_and_rebuilds_after_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let lines: Vec<&str> = CLAUDE.split_inclusive('\n').collect();
    std::fs::write(&path, lines[..3].concat()).unwrap();
    let t = LocalTransport;
    for use_exec in [false, true] {
        std::fs::write(&path, lines[..3].concat()).unwrap();
        let mut f = Follower::new(Harness::Claude, path.to_str().unwrap());
        assert!(f.poll_with(&t, use_exec).await.unwrap());
        assert_eq!(f.conversation().turns.len(), 1);
        assert!(!f.poll_with(&t, use_exec).await.unwrap(), "nothing new");
        std::fs::write(&path, CLAUDE).unwrap();
        assert!(f.poll_with(&t, use_exec).await.unwrap());
        assert_eq!(f.conversation().turns.len(), 2);
        // Rewritten shorter: rebuilt from the start.
        std::fs::write(&path, lines[..2].concat()).unwrap();
        assert!(f.poll_with(&t, use_exec).await.unwrap());
        assert_eq!(f.conversation().turns.len(), 1, "exec={use_exec}");
        assert_eq!(f.offset(), lines[..2].concat().len() as u64);
    }
}

/// Parses every real transcript on this machine. Run with
/// `cargo test -- --ignored parses_all_local_transcripts --nocapture`.
#[test]
#[ignore]
fn parses_all_local_transcripts() {
    let home = std::env::var("HOME").unwrap();
    let mut files: Vec<(Harness, std::path::PathBuf)> = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(
        &std::path::Path::new(&home).join(".claude/projects"),
        &mut v,
    );
    files.extend(
        v.drain(..)
            .filter(|p| !p.to_string_lossy().contains("/subagents/"))
            .map(|p| (Harness::Claude, p)),
    );
    walk(&std::path::Path::new(&home).join(".codex/sessions"), &mut v);
    files.extend(v.drain(..).map(|p| (Harness::Codex, p)));
    let (mut turns, mut answered, mut prompts) = (0, 0, 0);
    for (h, p) in &files {
        let bytes = std::fs::read(p).unwrap();
        let whole = {
            let mut tp = TranscriptParser::new(*h);
            tp.feed(&bytes);
            tp.conversation().clone()
        };
        let mut tp = TranscriptParser::new(*h);
        for c in bytes.chunks(4093) {
            tp.feed(c);
        }
        assert_eq!(tp.conversation(), &whole, "{}", p.display());
        turns += whole.turns.len();
        answered += whole.turns.iter().filter(|t| t.answer.is_some()).count();
        prompts += whole.turns.iter().filter(|t| t.prompt.is_some()).count();
        for t in &whole.turns {
            if let Some(pr) = &t.prompt {
                assert!(!pr.contains("<system-reminder>"), "{}: {pr}", p.display());
                assert!(
                    !pr.contains("<local-command-caveat>"),
                    "{}: {pr}",
                    p.display()
                );
                assert!(
                    !pr.contains("<environment_context>"),
                    "{}: {pr}",
                    p.display()
                );
            }
        }
    }
    println!(
        "{} files, {turns} turns, {prompts} with a prompt, {answered} with an answer",
        files.len()
    );
}
