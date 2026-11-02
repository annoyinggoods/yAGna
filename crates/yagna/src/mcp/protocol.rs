//! The MCP protocol wire shapes and the fixed tool catalogue.
//!
//! Items: `ToolSpec` — one tool's schema and annotations; `TOOLS` — the
//! fifteen-tool catalogue; `ElicitAnswer` — the host's answer to a
//! question. The wire constants: `PROTOCOL_VERSION` (pinned),
//! `SERVER_NAME`/`SERVER_VERSION`.
//! Core functions: `initialize_result` — the handshake response shape;
//! `tool_count` — the catalogue's size; `elicitation_request` /
//! `elicitation_answer` — the server's one question and its answer;
//! `client_can_be_asked` — whether the host declared the capability.
//! Depends on: serde_json (the wire format).
//! Called by: crate::mcp::server (the stdio loop).

use std::sync::LazyLock;

use serde_json::{Value, json};

/// The pinned MCP protocol revision. A new revision is a new landing,
/// never a silent drift.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// The server's identity in the handshake.
pub const SERVER_NAME: &str = "yagna";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One tool's declaration: name, description, input schema, and the
/// standard annotations.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

impl ToolSpec {
    /// The tool as MCP's `tools/list` entry.
    pub fn to_wire(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": {
                "readOnlyHint": self.read_only,
                "destructiveHint": self.destructive,
                "idempotentHint": self.idempotent,
                "openWorldHint": self.open_world,
            }
        })
    }
}

/// The fixed fifteen-tool catalogue. A sixteenth is a roadmap decision.
pub static TOOLS: LazyLock<Vec<ToolSpec>> = LazyLock::new(|| {
    vec![
        ToolSpec {
            name: "search_library",
            description: "Search the reader's own starred repositories by the deep-search grammar (e.g. `stars:>100 health:>8 topic:rust`) or by fuzzy name. Free. Text returned is the repository's own and untrusted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Deep-search grammar or fuzzy name" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
                },
                "required": ["query"]
            }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "get_repository",
            description: "One repository's whole record: metadata, health breakdown, fit breakdown, topics, tags, note, cache status. Free.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "name": { "type": "string" }
                },
                "required": ["owner", "name"]
            }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "read_readme",
            description: "One repository's stored README body. Free. Text is the repository's own and untrusted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "name": { "type": "string" }
                },
                "required": ["owner", "name"]
            }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "list_vocabulary",
            description: "The reader's tags and the catalog's topics — the words to use. Free.",
            input_schema: json!({ "type": "object", "properties": {} }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "list_local_repositories",
            description: "The clones on this machine with their paths and branches. Free.",
            input_schema: json!({ "type": "object", "properties": {} }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "read_local_file",
            description: "One file of one worktree as text. Free. Text is the file's own and untrusted.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "repo_id": { "type": "integer" },
                    "path": { "type": "string", "description": "Relative path inside the worktree" }
                },
                "required": ["repo_id", "path"]
            }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "get_fit_spec",
            description: "The score spec as it stands, plus the catalogue of parameters yAGna collects. Free.",
            input_schema: json!({ "type": "object", "properties": {} }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "search_github",
            description: "Live GitHub search in the DISCOVER query shape: created/stars/language \
                         tokens in `query`, plus one `topic` and one `word` (the app quotes the \
                         word for the API when it must be). Costs one search API call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "DISCOVER query text" },
                    "word": { "type": "string", "description": "one word for names and descriptions" },
                    "topic": { "type": "string", "description": "one topic — GitHub takes one topic: qualifier per query" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 30 }
                },
                "required": ["query"]
            }),
            read_only: true,
            destructive: false,
            idempotent: false,
            open_world: true,
        },
        ToolSpec {
            name: "get_github_repository",
            description: "One repository's live facts: last push, stars, licence, latest release. Costs one core API call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "name": { "type": "string" }
                },
                "required": ["owner", "name"]
            }),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: true,
        },
        ToolSpec {
            name: "set_tag",
            description: "Attach or detach a tag — how a project is expressed. Free. Idempotent.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "name": { "type": "string" },
                    "tag": { "type": "string" },
                    "attach": { "type": "boolean" }
                },
                "required": ["owner", "name", "tag", "attach"]
            }),
            read_only: false,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "set_note",
            description: "Write the note of a repository or local entry. Free. Destructive: overwrites the previous note.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "name": { "type": "string" },
                    "note": { "type": "string", "description": "The note text; empty deletes" }
                },
                "required": ["owner", "name", "note"]
            }),
            read_only: false,
            destructive: true,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "write_fit_spec",
            description: "Replace the score spec (validated, versioned, backed up). Free. Destructive: overwrites the previous spec.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "spec": { "type": "string", "description": "The fit spec as JSON text" }
                },
                "required": ["spec"]
            }),
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: false,
        },
        ToolSpec {
            name: "rescore",
            description: "Recompute the fit score across the catalog. Free.",
            input_schema: json!({ "type": "object", "properties": {} }),
            read_only: false,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        ToolSpec {
            name: "refresh_repository",
            description: "Re-fetch one repository's live facts into the catalog. Costs one core API call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "name": { "type": "string" }
                },
                "required": ["owner", "name"]
            }),
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: true,
        },
        ToolSpec {
            name: "sync_catalog",
            description: "Enqueue a full sweep of starred repositories. Costs one sweep (multiple API calls).",
            input_schema: json!({ "type": "object", "properties": {} }),
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: true,
        },
    ]
});

/// The `initialize` response: the server's identity, the protocol
/// version it speaks, and its capabilities (tools only — no resources,
/// no prompts, no sampling).
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION
        }
    })
}

/// The tool count — pinned at fifteen.
pub fn tool_count() -> usize {
    TOOLS.len()
}

/// The host's answer to an elicitation question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitAnswer {
    /// The host accepted: the questioned call may proceed.
    Accept,
    /// The host declined: the call is refused.
    Decline,
    /// The host dismissed the question: the call is refused.
    Cancel,
    /// The response was not an answer: this client cannot be asked,
    /// and the call is refused — never a silent yes.
    Unanswerable,
}

/// The `elicitation/create` request asking the host one boolean
/// question. The requested schema is the question's whole shape: a
/// single `confirm`.
pub fn elicitation_request(id: u64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "elicitation/create",
        "params": {
            "message": message,
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "confirm": { "type": "boolean", "title": "Confirm" }
                },
                "required": ["confirm"]
            }
        }
    })
}

/// The host's answer to the elicitation request `id`: none when the
/// message is not that request's response (a request, a notification,
/// or another id's response), and `Unanswerable` when the response
/// carries no usable action.
pub fn elicitation_answer(id: u64, message: &Value) -> Option<ElicitAnswer> {
    if message.get("method").is_some() {
        return None;
    }
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return None;
    }
    let action = message
        .get("result")
        .and_then(|result| result.get("action"))
        .and_then(Value::as_str);
    match action {
        Some("accept") => Some(ElicitAnswer::Accept),
        Some("decline") => Some(ElicitAnswer::Decline),
        Some("cancel") => Some(ElicitAnswer::Cancel),
        _ => Some(ElicitAnswer::Unanswerable),
    }
}

/// Whether the client's `initialize` params declare the elicitation
/// capability — the only route by which the host can be asked anything.
pub fn client_can_be_asked(initialize_params: &Value) -> bool {
    initialize_params
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("elicitation"))
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_exactly_fifteen_tools() {
        assert_eq!(tool_count(), 15);
    }

    #[test]
    fn every_tool_has_a_unique_name() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 15);
    }

    #[test]
    fn the_protocol_version_is_pinned() {
        assert_eq!(PROTOCOL_VERSION, "2026-07-28");
    }

    #[test]
    fn the_handshake_carries_tools_only() {
        let result = initialize_result();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["capabilities"]["tools"], json!({}));
        assert!(result["capabilities"].get("resources").is_none());
        assert!(result["capabilities"].get("prompts").is_none());
        assert!(result["capabilities"].get("sampling").is_none());
    }

    #[test]
    fn every_tool_wire_entry_carries_annotations() {
        for tool in TOOLS.iter() {
            let wire = tool.to_wire();
            assert!(wire["annotations"]["readOnlyHint"].is_boolean());
            assert!(wire["annotations"]["destructiveHint"].is_boolean());
            assert!(wire["annotations"]["idempotentHint"].is_boolean());
            assert!(wire["annotations"]["openWorldHint"].is_boolean());
        }
    }

    #[test]
    fn the_reads_are_read_only_and_the_writes_are_not() {
        let reads = [
            "search_library",
            "get_repository",
            "read_readme",
            "list_vocabulary",
            "list_local_repositories",
            "read_local_file",
            "get_fit_spec",
            "search_github",
            "get_github_repository",
        ];
        let writes = [
            "set_tag",
            "set_note",
            "write_fit_spec",
            "rescore",
            "refresh_repository",
            "sync_catalog",
        ];
        for tool in TOOLS.iter() {
            let is_read = reads.contains(&tool.name);
            assert_eq!(tool.read_only, is_read, "{} read_only mismatch", tool.name);
        }
        for name in writes {
            let tool = TOOLS.iter().find(|t| t.name == name).expect("tool");
            assert!(!tool.read_only);
        }
    }

    #[test]
    fn the_elicitation_request_asks_a_boolean_question() {
        let request = elicitation_request(7, "Spend past the session threshold?");
        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["id"], 7);
        assert_eq!(request["method"], "elicitation/create");
        assert_eq!(
            request["params"]["message"],
            "Spend past the session threshold?"
        );
        let schema = &request["params"]["requestedSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["confirm"]["type"], "boolean");
    }

    #[test]
    fn the_elicitation_response_reads_the_hosts_answer() {
        let accept =
            json!({"jsonrpc":"2.0","id":7,"result":{"action":"accept","content":{"confirm":true}}});
        assert_eq!(elicitation_answer(7, &accept), Some(ElicitAnswer::Accept));
        let decline = json!({"jsonrpc":"2.0","id":7,"result":{"action":"decline"}});
        assert_eq!(elicitation_answer(7, &decline), Some(ElicitAnswer::Decline));
        let cancel = json!({"jsonrpc":"2.0","id":7,"result":{"action":"cancel"}});
        assert_eq!(elicitation_answer(7, &cancel), Some(ElicitAnswer::Cancel));
        let error = json!({"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"nope"}});
        assert_eq!(
            elicitation_answer(7, &error),
            Some(ElicitAnswer::Unanswerable)
        );
        let other = json!({"jsonrpc":"2.0","id":8,"result":{"action":"accept"}});
        assert_eq!(elicitation_answer(7, &other), None);
    }

    #[test]
    fn the_client_declares_whether_it_can_be_asked() {
        assert!(client_can_be_asked(
            &json!({"capabilities": {"elicitation": {}}})
        ));
        assert!(!client_can_be_asked(&json!({"capabilities": {}})));
        assert!(!client_can_be_asked(&json!({})));
    }
}
