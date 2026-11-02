//! The MCP server: the stdio loop, the JSON-RPC dispatch, the tool
//! implementations, and the spend policy.
//!
//! Items: `protocol` — the wire shapes and the fifteen-tool catalogue;
//! `server` — the session, the stdio dispatch and the elicitation
//! round trip; `budget` — the spend policy and the session's ledger;
//! `trace` — the activity log; `tools` — the fifteen tools over the
//! store and forge facades.
//! Depends on: serde_json (the wire format), yagna_store (the disk
//! door), yagna_forge (the network door), crate::paths (data locations).
//! Called by: crate::main (the `mcp` verb).

pub mod budget;
pub mod protocol;
pub mod server;
pub mod tools;
pub mod trace;
