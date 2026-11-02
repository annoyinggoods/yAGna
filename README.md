# yAGna

**Your stars, offline** — a local-first library for the GitHub stars
catalog that outgrew the GitHub UI, and the only stars manager that
hands the whole collection to your coding agent over MCP.

Every repo you ever starred comes down to your disk and stays there:
READMEs with their images, licenses (the non-standard ones as full
text), topics, languages, file trees, the first and last commit, user
tags, notes, a twelve-point health index, fuzzy and full-text search,
and gitingest-style digests on demand. The GitHub stars page is a list;
yAGna treats your stars as a library worth keeping.

Built in **Rust** with **GPUI** (pinned exact) for the interface,
**octocrab** behind a `Forge` trait for gathering, **SQLite** with FTS5
for remembering, and **nucleo** for fuzzy matching. Windows-first, with
macOS and Linux kept green. Licensed under a Hippocratic-based
open-source license — the ethical restrictions are the point
([LICENSE.txt](LICENSE.txt)).

## Your agent reads your library

`yagna mcp` is a Model Context Protocol server over stdio, and it is
the part no other stars tool has. Not the browser extensions that dump
a JSON list, not the scripts that re-download starred repos into a
folder — nothing we know of serves your stars collection to a coding
agent as a live, searchable, scored library. The closest thing that
exists is a script that downloads a flat list of starred repositories,
or asking the GitHub API the same questions by hand every single time.

Point your MCP host at it:

```json
{
  "mcpServers": {
    "yagna": {
      "command": "npx",
      "args": ["-y", "yagna-mcp"]
    }
  }
}
```

or run the binary you already have with `yagna mcp`.

What the agent gets — fifteen tools, and the honest costs:

- **The reads are free.** `search_library` speaks the app's whole
  deep-search grammar (`stars:>100 health:>8 topic:rust`) and fuzzy
  names; `get_repository` answers with the full record — metadata,
  the health breakdown, the fit score, topics, tags, the note, what is
  cached; `read_readme` hands over the stored README;
  `list_vocabulary` gives the agent the reader's own tags and the
  catalog's topics so it does not invent words; `list_local_repositories`
  and `read_local_file` open your local worktrees; `get_fit_spec`
  shows the scoring spec an agent may rewrite against.
- **The writes are yours.** `set_tag`, `set_note`, `write_fit_spec`
  (validated, versioned, backed up), `rescore` — the agent organizes
  the library under your policy, and every call lands in an activity
  trace the app shows you with the agent's presence in the title bar.
- **The live calls pass a budget.** `search_github`,
  `get_github_repository`, `refresh_repository`, `sync_catalog` are
  charged against a spend ledger with per-hour and per-minute ceilings
  from `agent.toml`; past the confirmation threshold the server asks
  the host first — a host that cannot be asked gets a refusal, never a
  silent yes.

yAGna never acts on GitHub for you — no issues, no comments, no pushes.
It is the memory, not a second client.

## The honest pace

The first sweep of your stars takes **about five minutes per thousand
repositories** — GitHub's API limits set the speed, and yAGna is
serial with jitter because secondary rate limits bite. It is once:
after the sweep, everything reads from your disk, and updates bring
only what moved.

## Install

Download the Windows archive from the
[product page](https://annoyinggoods.com/products/yagna/), or build
from source — it is Rust and Cargo:

```
cargo build --release
```

- `cargo run -p yagna` — the app.
- `cargo run -p yagna -- sync` — the starred sweep into SQLite;
  `--enrich` works the README/license/tree queues.
- `cargo run -p yagna -- doctor` — the environment report: token,
  API reachability, data dir, schema version.
- `cargo run -p yagna -- mcp` — the MCP server (or `npx yagna-mcp`).

The token comes from `GITHUB_TOKEN` or `gh auth token`; it is never
logged and never leaves the machine. Your data lives under
`%APPDATA%\yagna\` and survives updates.

GitLab support is coming — the forge is built as a trait with GitHub
as the first tenant, and GitLab is the next one on the roadmap.

## The crates

One sentence of architecture: the forge pulls, the store keeps, the
app projects — and never the other way around.

| Crate | Owns | Never |
| --- | --- | --- |
| `yagna` | pixels and orchestration: windows, tabs, themes, the CLI verbs, the MCP server | opens the DB, dials the API |
| `yagna-core` | pure data and math: DTOs, health, fit, markdown, the query AST | files, clock, network, UI, DB |
| `yagna-store` | the disk: SQLite, FTS5, the asset cache, the queues | network, UI |
| `yagna-forge` | the network: the Forge trait, the runtime, rate limits | UI, the database |

The four discipline greps prove the boundaries and stay empty — see
[AGENTS.md](AGENTS.md) for the house rules those tests enforce.

## Releasing

`npm/yagna-mcp/` is the npx-installable MCP launcher, published as
[`yagna-mcp`](https://www.npmjs.com/package/yagna-mcp); it finds the
binary you have or fetches the release archive. The release archives
carry the binary, the license and an install note.
