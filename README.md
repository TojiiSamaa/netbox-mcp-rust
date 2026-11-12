# nbmcp — NetBox MCP server in Rust

> Fork of [netboxlabs/netbox-mcp-server](https://github.com/netboxlabs/netbox-mcp-server). The original Python server is kept in `src/` and documented in [README-python.md](README-python.md); this README covers the Rust server in `rust/`.

Rust port of `netbox-mcp-server`, with the part upstream deliberately left out: **writing**. One static binary, no Python runtime.

> Upstream policy (its `CLAUDE.md`) forbids writes and plugins. This fork overrides that on purpose. Writes are **off by default** and gated as described below.

## Build and run

```bash
cd rust && cargo build --release          # target/release/nbmcp
NETBOX_URL=https://netbox.example.com NETBOX_TOKEN=... ./nbmcp               # stdio
NETBOX_URL=... NETBOX_TOKEN=... ./nbmcp --transport http --port 8000        # HTTP, endpoint /mcp
./nbmcp --list-tools                                                          # tools visible with this config
```

## Configuration

Order: CLI flags > environment > `.env` in the current directory. Upstream variable names are kept.

| Variable | Default | Meaning |
|---|---|---|
| `NETBOX_URL`, `NETBOX_TOKEN` | required | Instance and API token (`nbt_…` v2 tokens are sent as Bearer, v1 as `Token`) |
| `VERIFY_SSL` | true | TLS verification |
| `ENABLE_PLUGIN_DISCOVERY` | true | Learn plugin types from `core/object-types` at startup |
| `LOG_LEVEL`, `CORS_ORIGINS`, `MCP_AUTH_TOKEN` | | As upstream (`NETBOX_MCP_TOKEN` also accepted) |
| `NETBOX_ALLOW_WRITES` | false | Enables create / update / allocate / raw POST-PUT-PATCH |
| `NETBOX_ALLOW_DELETE` | false | Also enables delete, bulk delete, scripts, raw DELETE (needs `ALLOW_WRITES`) |
| `NETBOX_DRY_RUN` | false | Every write is previewed (`would_send`), nothing is sent. Tools are listed even if writes are off |
| `NETBOX_WRITE_TYPES` | `*` | Writable types, patterns like `dcim.*,netbox_dns.zone`. `*` never matches `users.*` |
| `NETBOX_WRITE_DENY` | | Patterns refused even if allowed above |
| `NETBOX_ELICIT` | destructive | Ask the human before acting (`off`, `destructive`, `all`). **Fails closed**: if the client cannot be asked (no elicitation, or HTTP), the call is refused with `confirmation_required`. `off` accepts unconfirmed calls |
| `NETBOX_CONFIRM_ARG` | false | Opt-in fallback for clients that cannot be asked (HTTP, no elicitation): a destructive call is accepted when it carries `confirm: true`. The refusal tells the model to ask the user first. Off by default. The model writes `confirm` itself, so a prompt-injected agent can set it: prefer `NETBOX_APPROVAL_DIR` |
| `NETBOX_APPROVAL_DIR` | unset | Out-of-band approval for clients that cannot be asked (HTTP, no elicitation). A destructive call is refused with a one-time code; the operator runs `nbmcp approve <code>` in their own terminal (it shows the exact action and asks `y`; it refuses to run without a terminal), then the agent repeats the same call, which passes once within 10 minutes. `confirm: true` is ignored and not published when this is set. The directory must be out of the agent's reach (another account, or outside its sandbox): an agent that can write there can approve itself. `nbmcp approve` alone lists what is waiting |
| `NETBOX_MAX_BULK` / `NETBOX_MAX_LIMIT` | 100 / 100 | Items per bulk call / page size |
| `NETBOX_MAX_RESPONSE_CHARS` | 60000 | Answers are cut by rows, with `next_offset` |
| `NETBOX_REDACT_SECRETS` | true | Masks `preshared_key`, `psk`, `password`, `secret`… (and token keys for `users.token`) |
| `NETBOX_AUDIT_LOG` | | JSONL file: one line per write attempt (tool, args, outcome, requests sent) |
| `NETBOX_DISABLED_TOOLS` | | Comma list of tools to hide |
| `NETBOX_TOKEN_PASSTHROUGH` | false | HTTP: the client's `Authorization: Bearer` is its own NetBox token (NetBox enforces its permissions) |
| `NETBOX_MCP_ALLOWED_HOSTS`, `NETBOX_TIMEOUT` | | Extra `Host` values behind a proxy; request timeout (30 s) |

## Tools

**Read** (same names and parameters as upstream where they existed): `netbox_get_objects`, `netbox_get_object_by_id`, `netbox_get_changelogs`, `netbox_search_objects`. New: `netbox_list_object_types`, `netbox_describe_type` (fields, required, choices, filters, extra actions from the instance's own OpenAPI), `netbox_get_status`, `netbox_api_get`, `netbox_graphql` (queries only), `netbox_trace`, `netbox_get_available`, `netbox_rack_elevation`, `netbox_render_config`, `netbox_stats` (usage counters).

**Write**: `netbox_create_object`, `netbox_bulk_create`, `netbox_update_object` (returns `previous` to revert), `netbox_bulk_update`, `netbox_allocate` (next free IP / prefix / VLAN / ASN, race-free), `netbox_api_write`.

**Destructive**: `netbox_delete_object` and `netbox_bulk_delete` (both return a snapshot of what was removed), `netbox_run_script` (`commit=false` by default), `netbox_api_delete`.

Every write tool also takes `dry_run`.

## Test mode and stats

`MCP_TEST_MODE=1` (or `nbmcp --test`), set at startup only, answers every tool, writes and deletions included, from a simulated NetBox with fixtures. No URL or token needed, nothing leaves the process: the real HTTP client is never built, and any address other than the mock is cut with `TEST_MODE_EGRESS_BLOCKED`. Answers carry `test_mode: true` and `test_notice: "[TEST] ..."`.

`MCP_TEST_APPROVER=approve|deny|timeout|absent` (test mode only, default `deny`) simulates the human validation of write / destructive calls: approved (the mock runs), denied, timed out, or a client without elicitation (`confirmation_required`). Outside test mode it is ignored.

`netbox_stats` returns call counts, errors, p50 / p95 latency per tool and validation counts; `NETBOX_STATS_FILE=path.json` also exports them after each call.

## Plugins

Plugin models are first-class: with discovery on, they are added to the type list and every typed tool (`netbox_get_objects`, `create`, `update`, …) works on them (`netbox_dns.zone`). `netbox_describe_type` reads their fields and custom actions from OpenAPI. For anything else, `netbox_api_get` / `netbox_api_write` / `netbox_api_delete` reach any path under `/api` — they obey the same write switches and the same type policy (a path is mapped back to its type; unknown paths are refused when `NETBOX_WRITE_TYPES` is restrictive).

## Safety model

Writes off → separate delete switch → type allow/deny → optional human confirmation → dry-run → audit log. A refusal happens **before** any request is made. NetBox's own token permissions still apply on top. Anything NetBox returns is treated as untrusted data (the server's instructions tell the model so). Redirects are never followed (a redirected write would silently lose its body).

## Differences from upstream (fixed)

- Bulk operations use NetBox's real API (POST/PATCH/DELETE of a list on the list endpoint); there is no `/bulk/` path.
- `netbox_search_objects`: a type that fails is reported in `errors`; if the token is refused for every type, the call fails instead of answering "no results".
- `netbox_get_changelogs`: falls back to `extras/object-changes` on NetBox < 4.1; default `limit` 10 (entries carry full before/after data).
- Filters: `__in` and multi-hop lookups (`device__site_id`) are rejected with an explanation instead of returning silently wrong data; list values become repeated parameters.
- HTTP 4xx/5xx become typed errors with a hint and NetBox's per-field details; a JSON 404 (missing object) is told apart from an HTML 404 (unknown endpoint).
- Responses over the size limit are cut by rows with `next_offset`, never mid-JSON.

## Docker

Every release publishes a multi-arch image (linux/amd64, linux/arm64) on GHCR, built by `.github/workflows/release-rust.yml` in the same run as the GitHub Release, so the two always match:

```
docker pull ghcr.io/tojiisamaa/netbox-mcp-rust:latest
docker run --rm -p 8000:8000 -e NETBOX_URL=https://netbox.example.com -e NETBOX_TOKEN=xxx -e NETBOX_MCP_TOKEN=choose-a-secret ghcr.io/tojiisamaa/netbox-mcp-rust
```

The image serves HTTP on `/mcp`; outside loopback it needs `NETBOX_MCP_TOKEN` (or `NETBOX_TOKEN_PASSTHROUGH=true`). Writes stay off unless `NETBOX_ALLOW_WRITES=true`. For stdio: `docker run -i --rm -e NETBOX_URL=… -e NETBOX_TOKEN=… ghcr.io/tojiisamaa/netbox-mcp-rust --transport stdio`.

On loopback the HTTP mode answers without a token: set `NETBOX_MCP_TOKEN` as soon as anything else can reach the port.

To release: add the version's section to [`rust/CHANGELOG.md`](rust/CHANGELOG.md) (it becomes the Release notes; the workflow fails without it), bump `version` in `rust/Cargo.toml`, then either push the tag `vX.Y.Z` or run the workflow by hand (it creates the tag).

## Portable skill (any client)

`skill/netbox-mcp-rust/SKILL.md` documents all 24 tools, what is possible / impossible, and recipes. Install it by copying the folder into your client's skills location (Claude Code: `~/.claude/skills/`; OpenCode: `~/.config/opencode/skills/`; Cursor / Codex / Hermes: put its content in the rules or `AGENTS.md` mechanism of your client, this repo's `AGENTS.md` already points to it). `tests/test_skill_tools.py` fails if the skill and `rust/src/tools.rs` disagree.

## Claude Code skills

| Skill | File | Use |
|---|---|---|
| `nbmcp` | `.claude/skills/nbmcp/SKILL.md` | Using the Rust server safely: tools, write switches, confirmation, plugins |
| `netbox-mcp-testing` | `.claude/skills/netbox-mcp-testing/SKILL.md` | Upstream's protocol for testing the Python server against a live NetBox (kept as is) |

## Tests

`cargo test`: unit tests per module plus `tests/e2e.rs`, which drives the server over JSON-RPC against a fake NetBox that records every request (read-only by default, redaction, write flow with `previous`, delete snapshot, dry-run sends nothing, type policy blocks before any request, bulk and allocate paths, changelog fallback, plugin discovery, GraphQL mutation refused).
Test mode proofs: `tests/egress_static.rs` (no network / process call outside `src/client.rs`), `tests/test_mode.rs` (all tools answer from the mock under a panicking transport; the binary run against a sentinel listener receives 0 connections; the 4 validation scenarios for each of the 10 write / destructive tools). For OS-level isolation run `docker run --rm --network none -e NBMCP_ISOLATED=1 ... cargo test --offline` (CI does it). `rust/scripts/mutation-check.sh` adds a real network call outside the single point and requires the static and binary proofs to fail.
Not tested: a real NetBox instance (the fake follows the documented API, but only a run against NetBox proves it).
