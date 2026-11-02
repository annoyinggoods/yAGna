//! The agent's activity trace (`<data>/agent-activity.jsonl`): the
//! session's receipt, one JSON object per line.
//!
//! The server writes a session line at connect, a heartbeat while it
//! lives, and one action line per tool call (the tool, the target, the
//! outcome, the API cost). The trace is also how the app knows an agent
//! is there: the status line reads the heartbeat's age, the activity
//! drawer reads the action lines — both through `read_tail`.
//!
//! Items: `Trace` — the appending writer; `TraceLine` — one line;
//! `TraceKind` — session, heartbeat or action.
//! Core functions: `read_tail` — the bounded tail the app renders.
//! Depends on: chrono (the instants), serde_json (the lines).
//! Called by: crate::mcp::server (writing) and crate::ui (reading).

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::budget::SpendKind;

/// The newest part of the file the reader considers: a busy session's
/// trace grows without bound, and the app only ever renders its tail.
const TAIL_MAX_BYTES: u64 = 256 * 1024;

/// The trace's file name inside the data directory — one name, one
/// definition, for the server that writes it and the app that reads it.
pub const TRACE_FILE: &str = "agent-activity.jsonl";

/// What one line of the trace is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TraceKind {
    /// The server's session began.
    Session,
    /// The server is still there.
    Heartbeat,
    /// One tool call happened.
    Action,
}

/// What a tool call came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionOutcome {
    Ok,
    Error,
    Refused,
}

/// One line of the trace: the instant, the kind, and — for actions —
/// the tool, the target, the outcome and the API cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceLine {
    pub at: DateTime<Utc>,
    pub kind: TraceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
}

impl TraceLine {
    /// The session-start line.
    pub fn session(at: DateTime<Utc>) -> Self {
        Self {
            at,
            kind: TraceKind::Session,
            tool: None,
            subject: None,
            outcome: None,
            cost: None,
        }
    }

    /// The heartbeat line — the app reads its age.
    pub fn heartbeat(at: DateTime<Utc>) -> Self {
        Self {
            at,
            kind: TraceKind::Heartbeat,
            tool: None,
            subject: None,
            outcome: None,
            cost: None,
        }
    }

    /// One tool call's line.
    pub fn action(
        at: DateTime<Utc>,
        tool: &str,
        subject: &str,
        outcome: ActionOutcome,
        cost: Option<SpendKind>,
    ) -> Self {
        Self {
            at,
            kind: TraceKind::Action,
            tool: Some(tool.to_string()),
            subject: (!subject.is_empty()).then(|| subject.to_string()),
            outcome: Some(
                match outcome {
                    ActionOutcome::Ok => "ok",
                    ActionOutcome::Error => "error",
                    ActionOutcome::Refused => "refused",
                }
                .to_string(),
            ),
            cost: Some(
                match cost {
                    None => "none",
                    Some(SpendKind::Core) => "core",
                    Some(SpendKind::Search) => "search",
                }
                .to_string(),
            ),
        }
    }
}

/// The appending writer: one JSON object per line, flushed as it goes.
pub struct Trace {
    file: std::fs::File,
}

impl Trace {
    /// Opens (creating when needed) the trace for appending.
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("agent trace {}: {error}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| format!("agent trace {}: {error}", path.display()))?;
        Ok(Self { file })
    }

    /// Appends one line and flushes it.
    pub fn append(&mut self, line: &TraceLine) -> Result<(), String> {
        let text =
            serde_json::to_string(line).map_err(|error| format!("agent trace line: {error}"))?;
        self.file
            .write_all(text.as_bytes())
            .and_then(|_| self.file.write_all(b"\n"))
            .and_then(|_| self.file.flush())
            .map_err(|error| format!("agent trace write: {error}"))
    }
}

/// The newest whole lines of the trace, oldest first among the kept.
/// A missing file reads as nothing; a half-written line is skipped —
/// the reader wants the receipt, not the crash.
pub fn read_tail(path: &Path, max_lines: usize) -> Vec<TraceLine> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let end = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = end.saturating_sub(TAIL_MAX_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buffer = String::new();
    if file
        .take(TAIL_MAX_BYTES)
        .read_to_string(&mut buffer)
        .is_err()
    {
        return Vec::new();
    }
    let whole = cut_partial_first_line(&buffer, start);
    let mut lines: Vec<TraceLine> = whole
        .lines()
        .filter_map(|text| serde_json::from_str(text).ok())
        .collect();
    let kept = lines.len().saturating_sub(max_lines);
    lines.split_off(kept)
}

/// The buffer with its first line dropped when the read started
/// mid-file — that first line is the middle of someone's JSON.
fn cut_partial_first_line(buffer: &str, start: u64) -> &str {
    if start == 0 {
        return buffer;
    }
    buffer.split_once('\n').map_or("", |(_, rest)| rest)
}

/// The status line's words for the newest heartbeat's age: an agent is
/// connected while the heartbeat is fresh, and was connected when it
/// is not. Nothing when the trace holds no heartbeat at all.
pub fn presence_line(lines: &[TraceLine], now: DateTime<Utc>) -> Option<String> {
    let newest = lines
        .iter()
        .filter(|line| line.kind == TraceKind::Heartbeat)
        .max_by_key(|line| line.at)?;
    let age = now - newest.at;
    if age <= chrono::Duration::seconds(90) {
        return Some("an agent is connected".to_string());
    }
    let minutes = age.num_minutes().max(1);
    let noun = if minutes == 1 { "minute" } else { "minutes" };
    Some(format!("an agent was connected {minutes} {noun} ago"))
}

/// One action line as the activity drawer's row: the tool, its target,
/// the outcome, and the API cost when the call spent one.
pub fn action_label(line: &TraceLine) -> String {
    if line.kind != TraceKind::Action {
        return String::new();
    }
    let tool = line.tool.as_deref().unwrap_or("?");
    let mut parts = vec![tool.to_string()];
    if let Some(subject) = line.subject.as_deref() {
        parts.push(subject.to_string());
    }
    if let Some(outcome) = line.outcome.as_deref() {
        parts.push(outcome.to_string());
    }
    if line.cost.as_deref().is_some_and(|cost| cost != "none") {
        parts.push(format!(
            "costs the {} budget",
            line.cost.as_deref().unwrap_or_default()
        ));
    }
    parts.join(" · ")
}

/// The app-side summary of the trace: the presence line and the newest
/// action rows, shaped for display and newest first. The server writes
/// the file; this reads it.
pub fn activity_summary(path: &Path, now: DateTime<Utc>) -> (Option<String>, Vec<String>) {
    let lines = read_tail(path, 40);
    let presence = presence_line(&lines, now);
    let mut rows: Vec<String> = lines
        .iter()
        .filter(|line| line.kind == TraceKind::Action)
        .map(action_label)
        .collect();
    rows.reverse();
    rows.truncate(20);
    (presence, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("yagna-trace-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn at(second: u64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_762_000_000 + second as i64, 0).expect("timestamp")
    }

    #[test]
    fn a_session_line_and_a_heartbeat_land_in_the_jsonl() {
        let dir = scratch("session");
        let path = dir.join("agent-activity.jsonl");
        let mut trace = Trace::open(&path).expect("open");
        trace.append(&TraceLine::session(at(0))).expect("session");
        trace
            .append(&TraceLine::heartbeat(at(1)))
            .expect("heartbeat");
        let lines = read_tail(&path, 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].kind, TraceKind::Session);
        assert_eq!(lines[0].at, at(0));
        assert_eq!(lines[1].kind, TraceKind::Heartbeat);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_action_line_carries_the_tool_the_subject_and_the_outcome() {
        let dir = scratch("action");
        let path = dir.join("agent-activity.jsonl");
        let mut trace = Trace::open(&path).expect("open");
        trace
            .append(&TraceLine::action(
                at(0),
                "search_library",
                "stars:>100",
                ActionOutcome::Ok,
                Some(SpendKind::Search),
            ))
            .expect("action");
        let lines = read_tail(&path, 10);
        let line = &lines[0];
        assert_eq!(line.kind, TraceKind::Action);
        assert_eq!(line.tool.as_deref(), Some("search_library"));
        assert_eq!(line.subject.as_deref(), Some("stars:>100"));
        assert_eq!(line.outcome.as_deref(), Some("ok"));
        assert_eq!(line.cost.as_deref(), Some("search"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tail_reader_keeps_the_newest_lines() {
        let dir = scratch("tail");
        let path = dir.join("agent-activity.jsonl");
        let mut trace = Trace::open(&path).expect("open");
        for second in 0..5 {
            trace
                .append(&TraceLine::action(
                    at(second),
                    "rescore",
                    "",
                    ActionOutcome::Ok,
                    None,
                ))
                .expect("action");
        }
        let lines = read_tail(&path, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].at, at(3));
        assert_eq!(lines[1].at, at(4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_trace_file_reads_as_nothing() {
        let dir = scratch("missing");
        let lines = read_tail(&dir.join("nope.jsonl"), 10);
        assert!(lines.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_half_written_last_line_is_skipped_not_fatal() {
        let dir = scratch("half");
        let path = dir.join("agent-activity.jsonl");
        let mut trace = Trace::open(&path).expect("open");
        trace
            .append(&TraceLine::heartbeat(at(0)))
            .expect("heartbeat");
        drop(trace);
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        file.write_all(b"{\"at\":\"2026-").expect("half line");
        let lines = read_tail(&path, 10);
        assert_eq!(lines.len(), 1, "the whole line stands, the half is skipped");
        assert_eq!(lines[0].kind, TraceKind::Heartbeat);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_presence_line_reads_the_newest_heartbeat() {
        let lines = vec![
            TraceLine::heartbeat(at(0)),
            TraceLine::action(at(1), "rescore", "", ActionOutcome::Ok, None),
            TraceLine::heartbeat(at(10)),
        ];
        assert_eq!(
            presence_line(&lines, at(20)).as_deref(),
            Some("an agent is connected")
        );
        assert_eq!(
            presence_line(&lines, at(200)).as_deref(),
            Some("an agent was connected 3 minutes ago")
        );
        assert_eq!(
            presence_line(&lines, at(130)).as_deref(),
            Some("an agent was connected 2 minutes ago")
        );
        assert_eq!(presence_line(&[], at(0)), None);
    }

    #[test]
    fn an_action_row_names_the_tool_the_target_and_the_cost() {
        let spent = TraceLine::action(
            at(0),
            "search_github",
            "stars:>100",
            ActionOutcome::Ok,
            Some(SpendKind::Search),
        );
        assert_eq!(
            action_label(&spent),
            "search_github · stars:>100 · ok · costs the search budget"
        );
        let free = TraceLine::action(
            at(0),
            "get_repository",
            "owner/name",
            ActionOutcome::Refused,
            None,
        );
        assert_eq!(action_label(&free), "get_repository · owner/name · refused");
    }

    #[test]
    fn the_activity_summary_shapes_the_rows_newest_first() {
        let dir = scratch("summary");
        let path = dir.join(TRACE_FILE);
        let mut trace = Trace::open(&path).expect("open");
        trace.append(&TraceLine::heartbeat(at(0))).expect("h");
        trace
            .append(&TraceLine::action(
                at(1),
                "rescore",
                "",
                ActionOutcome::Ok,
                None,
            ))
            .expect("a1");
        trace
            .append(&TraceLine::action(
                at(2),
                "search_library",
                "stars:>10",
                ActionOutcome::Ok,
                None,
            ))
            .expect("a2");
        drop(trace);
        let (presence, rows) = activity_summary(&path, at(10));
        assert_eq!(presence.as_deref(), Some("an agent is connected"));
        assert_eq!(rows[0], "search_library · stars:>10 · ok");
        assert_eq!(rows[1], "rescore · ok");
        let (presence, rows) = activity_summary(&dir.join("nope.jsonl"), at(10));
        assert_eq!(presence, None);
        assert!(rows.is_empty(), "no trace, no rows");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
