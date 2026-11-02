//! The MCP server session: the JSON-RPC dispatch over a stream pair,
//! the tool calls through the spend policy, and the one question the
//! server ever asks the host.
//!
//! `Session` owns the reader, the writer, the catalog, the token, the
//! data directory, the spend ledger and the activity trace. A tool
//! call is charged against the ledger before it runs: past the policy's
//! confirmation threshold the host is asked first — through
//! `elicitation/create`, in this same line loop — and a host that
//! cannot be asked gets a refusal that names the ceiling and the file,
//! never a silent yes. Every tool call lands one action line in the
//! trace; a heartbeat thread keeps the trace alive while the session
//! idles, which is how the app knows an agent is connected.
//!
//! Items: `Session` — the whole server state and its line loop.
//! Core functions: `run` — the verb's entry; `Session::serve` — the
//! loop; `Session::handle_raw` — one line to its response;
//! `Session::ask_host` — the elicitation round trip.
//! Depends on: serde_json (the wire format), crate::mcp::protocol (the
//! shapes), crate::mcp::tools (the fifteen tools), crate::mcp::budget
//! (the ledger), crate::mcp::trace (the receipt), crate::paths,
//! crate::token.
//! Called by: crate::main (the `mcp` verb).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde_json::{Value, json};

#[cfg(test)]
use super::budget::SpendPolicy;
use super::budget::{Admission, Budget, SpendKind, load_policy};
use super::protocol::{self, ElicitAnswer};
use super::tools::{self, ToolContext};
use super::trace::{ActionOutcome, Trace, TraceLine};

/// Where the server's elicitation ids start: the client numbers its
/// own requests from the low end, so the two directions never collide.
const ELICITATION_ID_BASE: u64 = 1000;

/// How often the heartbeat thread says the session is still there.
const HEARTBEAT_SECS: u64 = 30;

/// The server's whole state over one stream pair.
pub struct Session<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    store: yagna_store::Store,
    token: Option<String>,
    data_dir: PathBuf,
    budget: Arc<Mutex<Budget>>,
    trace: Arc<Mutex<Trace>>,
    sweeping: Arc<AtomicBool>,
    /// Whether the client declared the elicitation capability.
    can_ask: bool,
    /// Whether a host question is in flight — one at a time, so a
    /// nested ask is refused rather than tangled.
    asking: bool,
    next_elicitation_id: u64,
}

impl<R: BufRead, W: Write> Session<R, W> {
    /// A session over its stream pair and its backends.
    pub fn new(
        reader: R,
        writer: W,
        store: yagna_store::Store,
        token: Option<String>,
        data_dir: PathBuf,
        budget: Budget,
        trace: Trace,
    ) -> Self {
        Self {
            reader,
            writer,
            store,
            token,
            data_dir,
            budget: Arc::new(Mutex::new(budget)),
            trace: Arc::new(Mutex::new(trace)),
            sweeping: Arc::new(AtomicBool::new(false)),
            can_ask: false,
            asking: false,
            next_elicitation_id: ELICITATION_ID_BASE,
        }
    }

    /// Serves until the stream ends: one JSON-RPC message per line in,
    /// one response per request out. A heartbeat thread keeps the trace
    /// fresh while the session waits on input.
    pub fn serve(&mut self) -> std::process::ExitCode {
        self.append(&TraceLine::session(Utc::now()));
        let alive = Arc::new(AtomicBool::new(true));
        let heartbeat = self.start_heartbeat(&alive);
        let outcome = self.loop_lines();
        alive.store(false, Ordering::Relaxed);
        let _ = heartbeat.join();
        match outcome {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(()) => std::process::ExitCode::FAILURE,
        }
    }

    fn loop_lines(&mut self) -> Result<(), ()> {
        loop {
            let Some(line) = self.read_line() else {
                return Ok(());
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if self.respond_raw(trimmed).is_err() {
                return Err(());
            }
        }
    }

    /// Handles one raw line and writes its response, when there is one.
    fn respond_raw(&mut self, trimmed: &str) -> Result<(), ()> {
        match self.handle_raw(trimmed) {
            Some(response) => self.respond(&response),
            None => Ok(()),
        }
    }

    fn start_heartbeat(&self, alive: &Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
        let trace = Arc::clone(&self.trace);
        let alive = Arc::clone(alive);
        std::thread::spawn(move || {
            while alive.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(HEARTBEAT_SECS));
                if !alive.load(Ordering::Relaxed) {
                    return;
                }
                if let Ok(mut trace) = trace.lock() {
                    let _ = trace.append(&TraceLine::heartbeat(Utc::now()));
                }
            }
        })
    }

    fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }

    fn respond(&mut self, response: &Value) -> Result<(), ()> {
        let text = serde_json::to_string(response).unwrap_or_default();
        if writeln!(self.writer, "{text}").is_err() {
            return Err(());
        }
        if self.writer.flush().is_err() {
            return Err(());
        }
        Ok(())
    }

    /// One raw line to its response (or none for notifications).
    pub fn handle_raw(&mut self, raw: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(raw) {
            Ok(value) => value,
            Err(_) => return Some(json_rpc_error(Value::Null, -32700, "parse error")),
        };
        self.handle_message(&message)
    }

    /// One parsed message to its response (or none for notifications).
    pub fn handle_message(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                self.can_ask = protocol::client_can_be_asked(&params);
                Some(json_rpc_result(id, protocol::initialize_result()))
            }
            "notifications/initialized" => None,
            "tools/list" => Some(json_rpc_result(id, tools_list_result())),
            "tools/call" => Some(self.handle_tools_call(id, message)),
            "ping" => Some(json_rpc_result(id, json!({}))),
            _ => Some(json_rpc_error(
                id.unwrap_or(Value::Null),
                -32601,
                &format!("method not found: {method}"),
            )),
        }
    }

    /// The `tools/call` handler: the spend gate first, then the tool,
    /// then the trace line. Every result is a structured content block.
    fn handle_tools_call(&mut self, id: Option<Value>, message: &Value) -> Value {
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        let subject = subject_of(&name, &arguments);
        let cost = tools::spend_kind(&name);
        if let Admission::Refused { message } = self.admit_tool(&name) {
            self.record(&name, &subject, ActionOutcome::Refused, cost);
            return json_rpc_result(id, err_result(&message));
        }
        let mut context = self.tool_context();
        let outcome = tools::execute(&name, &arguments, &mut context);
        drop(context);
        let result = match outcome {
            Ok(value) => {
                self.record(&name, &subject, ActionOutcome::Ok, cost);
                ok_result(value)
            }
            Err(message) => {
                self.record(&name, &subject, ActionOutcome::Error, cost);
                err_result(&message)
            }
        };
        json_rpc_result(id, result)
    }

    /// The spend gate for one tool call: nothing for the free tools, a
    /// charged call for the live ones, and at the threshold the host's
    /// answer first.
    fn admit_tool(&mut self, name: &str) -> Admission {
        let Some(kind) = tools::spend_kind(name) else {
            return Admission::Admitted;
        };
        match self.admit(kind) {
            Admission::Confirm { message } => self.resolve_confirmation(&message, kind),
            other => other,
        }
    }

    fn admit(&self, kind: SpendKind) -> Admission {
        let mut budget = lock_budget(&self.budget);
        budget.admit(kind, Utc::now())
    }

    /// The threshold's question to the host, and what its answer means
    /// for the waiting call.
    fn resolve_confirmation(&mut self, message: &str, kind: SpendKind) -> Admission {
        let answer = self.ask_host(message);
        let mut budget = lock_budget(&self.budget);
        match answer {
            ElicitAnswer::Accept => {
                budget.approve_crossing();
                budget.admit(kind, Utc::now())
            }
            ElicitAnswer::Decline | ElicitAnswer::Cancel => {
                budget.decline_crossing();
                Admission::Refused {
                    message: budget.declined_message(),
                }
            }
            ElicitAnswer::Unanswerable => {
                budget.decline_crossing();
                Admission::Refused {
                    message: budget.unaskable_message(),
                }
            }
        }
    }

    /// The server's one question: `elicitation/create` out, the
    /// matching response back. Anything else the client sends while the
    /// question is open is handled inline — a question never blocks the
    /// rest of the protocol. A client that cannot be asked is never
    /// given a silent yes.
    pub fn ask_host(&mut self, message: &str) -> ElicitAnswer {
        if !self.can_ask || self.asking {
            return ElicitAnswer::Unanswerable;
        }
        self.asking = true;
        let outcome = self.ask_host_inner(message);
        self.asking = false;
        outcome
    }

    fn ask_host_inner(&mut self, message: &str) -> ElicitAnswer {
        self.next_elicitation_id += 1;
        let id = self.next_elicitation_id;
        let request = protocol::elicitation_request(id, message);
        if self.respond(&request).is_err() {
            return ElicitAnswer::Unanswerable;
        }
        loop {
            let Some(line) = self.read_line() else {
                return ElicitAnswer::Unanswerable;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(answer) = self.elicitation_answer(id, trimmed) {
                return answer;
            }
            if self.respond_raw(trimmed).is_err() {
                return ElicitAnswer::Unanswerable;
            }
        }
    }

    fn elicitation_answer(&self, id: u64, raw: &str) -> Option<ElicitAnswer> {
        let message: Value = serde_json::from_str(raw).ok()?;
        protocol::elicitation_answer(id, &message)
    }

    fn tool_context(&mut self) -> ToolContext<'_> {
        ToolContext {
            store: &mut self.store,
            token: self.token.clone(),
            data_dir: self.data_dir.clone(),
            budget: Arc::clone(&self.budget),
            trace: Arc::clone(&self.trace),
            sweeping: Arc::clone(&self.sweeping),
        }
    }

    fn record(&self, tool: &str, subject: &str, outcome: ActionOutcome, cost: Option<SpendKind>) {
        let line = TraceLine::action(Utc::now(), tool, subject, outcome, cost);
        self.append(&line);
    }

    fn append(&self, line: &TraceLine) {
        if let Ok(mut trace) = self.trace.lock() {
            let _ = trace.append(line);
        }
    }
}

/// The budget lock: a poisoned ledger is still a ledger.
fn lock_budget(budget: &Mutex<Budget>) -> std::sync::MutexGuard<'_, Budget> {
    budget.lock().unwrap_or_else(|error| error.into_inner())
}

/// Runs the server on the process's own streams: the catalog and the
/// data directory from the app's locations, the token the environment
/// carries (live tools refuse without one), the policy and the trace
/// beside the catalog.
pub fn run() -> std::process::ExitCode {
    let Ok(data_dir) = crate::paths::data_dir() else {
        eprintln!("mcp: the data directory is unavailable");
        return std::process::ExitCode::FAILURE;
    };
    let store = match crate::paths::open_catalog() {
        Ok(store) => store,
        Err(problem) => {
            eprintln!("mcp: {problem}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (policy, loud) = load_policy(&data_dir.join("agent.toml"));
    if let Some(line) = loud {
        eprintln!("{line}");
    }
    let trace = match Trace::open(&data_dir.join(super::trace::TRACE_FILE)) {
        Ok(trace) => trace,
        Err(problem) => {
            eprintln!("mcp: {problem}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let token = crate::token::acquire(false);
    let budget = Budget::new(policy, data_dir.join("agent.toml"));
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut session = Session::new(
        stdin.lock(),
        stdout.lock(),
        store,
        token,
        data_dir,
        budget,
        trace,
    );
    session.serve()
}

/// The `tools/list` response body.
fn tools_list_result() -> Value {
    debug_assert_eq!(protocol::tool_count(), 15);
    let tools: Vec<Value> = protocol::TOOLS.iter().map(|t| t.to_wire()).collect();
    json!({ "tools": tools })
}

/// The target one tool call works on, for the trace line: the
/// repository identity when there is one, the query when there is one.
fn subject_of(name: &str, arguments: &Value) -> String {
    let owner = arguments.get("owner").and_then(Value::as_str);
    let repo = arguments.get("name").and_then(Value::as_str);
    if let (Some(owner), Some(repo)) = (owner, repo) {
        return format!("{owner}/{repo}");
    }
    if let Some(query) = arguments.get("query").and_then(Value::as_str) {
        return query.to_string();
    }
    if let Some(tag) = arguments.get("tag").and_then(Value::as_str) {
        return tag.to_string();
    }
    name.to_string()
}

/// A tool result with content.
fn ok_result(value: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
        "isError": false
    })
}

/// A tool result carrying an error.
fn err_result(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true
    })
}

/// Builds a JSON-RPC result envelope.
fn json_rpc_result(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    })
}

/// Builds a JSON-RPC error envelope.
fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("yagna-server-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    type TestSession = Session<Cursor<Vec<u8>>, Vec<u8>>;

    /// A session over empty input and a collecting writer, with the
    /// default policy and no token — everything in one temp directory.
    fn session(label: &str) -> (Scratch, TestSession) {
        let scratch = Scratch::new(label);
        let data_dir = scratch.path();
        let store = yagna_store::Store::open(&data_dir.join("catalog.db")).expect("open");
        let trace = Trace::open(&data_dir.join(crate::mcp::trace::TRACE_FILE)).expect("trace");
        let session = Session::new(
            Cursor::new(Vec::new()),
            Vec::new(),
            store,
            None,
            data_dir,
            Budget::new(
                SpendPolicy::default(),
                std::path::PathBuf::from("agent.toml"),
            ),
            trace,
        );
        (scratch, session)
    }

    /// A session whose policy asks the host before the first spent
    /// call — the elicitation seam.
    fn asking_session(label: &str) -> (Scratch, TestSession) {
        let scratch = Scratch::new(label);
        let data_dir = scratch.path();
        let store = yagna_store::Store::open(&data_dir.join("catalog.db")).expect("open");
        let trace = Trace::open(&data_dir.join(crate::mcp::trace::TRACE_FILE)).expect("trace");
        let policy = SpendPolicy {
            core_per_hour: 500,
            search_per_minute: 10,
            confirm_above: 0,
        };
        let session = Session::new(
            Cursor::new(Vec::new()),
            Vec::new(),
            store,
            None,
            data_dir,
            Budget::new(policy, std::path::PathBuf::from("agent.toml")),
            trace,
        );
        (scratch, session)
    }

    /// Feeds the given lines through the loop and returns every JSON
    /// object the session wrote — responses and host questions alike.
    fn answers(session: &mut TestSession, lines: &[&str]) -> Vec<Value> {
        session
            .reader
            .get_mut()
            .extend_from_slice(lines.join("\n").as_bytes());
        session.reader.get_mut().extend_from_slice(b"\n");
        session.reader.set_position(0);
        while let Some(line) = session.read_line() {
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(response) = session.handle_raw(&trimmed) {
                let _ = session.respond(&response);
            }
        }
        let written = String::from_utf8_lossy(&session.writer).to_string();
        written
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    #[test]
    fn initialize_returns_the_handshake_shape() {
        let (_scratch, mut session) = session("initialize");
        let response = session
            .handle_raw(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
            .unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["protocolVersion"], "2026-07-28");
    }

    #[test]
    fn initialized_notification_gets_no_response() {
        let (_scratch, mut session) = session("initialized");
        let response =
            session.handle_raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(response.is_none());
    }

    #[test]
    fn tools_list_returns_fifteen_tools() {
        let (_scratch, mut session) = session("tools-list");
        let response = session
            .handle_raw(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 15);
    }

    #[test]
    fn ping_returns_an_empty_result() {
        let (_scratch, mut session) = session("ping");
        let response = session
            .handle_raw(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#)
            .unwrap();
        assert_eq!(response["result"], json!({}));
    }

    #[test]
    fn unknown_method_returns_error_minus_32601() {
        let (_scratch, mut session) = session("unknown");
        let response = session
            .handle_raw(r#"{"jsonrpc":"2.0","id":4,"method":"bogus"}"#)
            .unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn malformed_json_returns_parse_error() {
        let (_scratch, mut session) = session("malformed");
        let response = session.handle_raw("not json").unwrap();
        assert_eq!(response["error"]["code"], -32700);
    }

    #[test]
    fn tools_call_dispatches_by_name() {
        let (_scratch, mut session) = session("call");
        let raw = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#;
        let response = session.handle_raw(raw).unwrap();
        assert!(response["result"].is_object());
    }

    #[test]
    fn get_fit_spec_returns_the_catalogue() {
        let (_scratch, mut session) = session("fit-spec");
        let raw = r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_fit_spec","arguments":{}}}"#;
        let response = session.handle_raw(raw).unwrap();
        assert_eq!(response["result"]["isError"], false);
    }

    #[test]
    fn every_tool_call_lands_an_action_line_in_the_trace() {
        let (scratch, mut session) = session("trace-action");
        let raw = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_fit_spec","arguments":{}}}"#;
        session.handle_raw(raw).unwrap();
        let lines =
            super::super::trace::read_tail(&scratch.path().join(crate::mcp::trace::TRACE_FILE), 10);
        let action = lines
            .iter()
            .find(|line| line.kind == super::super::trace::TraceKind::Action)
            .expect("an action line");
        assert_eq!(action.tool.as_deref(), Some("get_fit_spec"));
        assert_eq!(action.outcome.as_deref(), Some("ok"));
        assert_eq!(action.cost.as_deref(), Some("none"));
    }

    #[test]
    fn the_threshold_asks_the_host_and_honors_a_decline() {
        let (_scratch, mut session) = asking_session("decline");
        session.token = Some("test-token".to_string());
        session.can_ask = true;
        let lines = answers(
            &mut session,
            &[
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_github","arguments":{"query":"stars:10"}}}"#,
                r#"{"jsonrpc":"2.0","id":1001,"result":{"action":"decline"}}"#,
            ],
        );
        // The elicitation question went out first, then the refused
        // tool result.
        let asked = lines
            .iter()
            .find(|line| line["method"] == "elicitation/create")
            .expect("the question is asked");
        assert!(
            asked["params"]["message"]
                .as_str()
                .expect("message")
                .contains("confirm_above")
        );
        let result = lines
            .iter()
            .find(|line| line["id"] == 2)
            .expect("the tool result");
        assert_eq!(result["result"]["isError"], true);
        let text = result["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(
            text.contains("agent.toml"),
            "the refusal names the file: {text}"
        );
    }

    #[test]
    fn a_host_that_cannot_be_asked_is_refused_the_ceiling() {
        let (_scratch, mut session) = asking_session("unaskable");
        let lines = answers(
            &mut session,
            &[
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_github","arguments":{"query":"stars:10"}}}"#,
            ],
        );
        let result = lines
            .iter()
            .find(|line| line["id"] == 2)
            .expect("the tool result");
        assert_eq!(result["result"]["isError"], true);
        let text = result["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(
            text.contains("confirm_above"),
            "the refusal names the ceiling: {text}"
        );
        assert!(text.contains("agent.toml"), "and the file: {text}");
    }

    #[test]
    fn a_tool_call_never_hits_the_network_without_a_token() {
        let (_scratch, mut session) = asking_session("no-token");
        session.can_ask = true;
        let lines = answers(
            &mut session,
            &[
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_github","arguments":{"query":"stars:10"}}}"#,
                r#"{"jsonrpc":"2.0","id":1001,"result":{"action":"accept"}}"#,
            ],
        );
        let result = lines
            .iter()
            .find(|line| line["id"] == 2)
            .expect("the tool result");
        // The host said yes and the call went through the gate — the
        // tool itself then refuses for the missing token, offline.
        assert_eq!(result["result"]["isError"], true);
        let text = result["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(
            text.contains("token"),
            "the tool refuses without a token: {text}"
        );
    }
}
