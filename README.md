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
| `NETBOX_MAX_BULK` / `NETBOX_MAX_LIMIT` | 100 / 100 | Items per bulk call / page size |
| `NETBOX_MAX_RESPONSE_CHARS` | 60000 | Answers are cut by rows, with `next_offset` |
| `NETBOX_REDACT_SECRETS` | true | Masks `preshared_key`, `psk`, `password`, `secret`… (and token keys for `users.token`) |
| `NETBOX_AUDIT_LOG` | | JSONL file: one line per write attempt (tool, args, outcome, requests sent) |
| `NETBOX_DISABLED_TOOLS` | | Comma list of tools to hide |
| `NETBOX_TOKEN_PASSTHROUGH` | false | HTTP: the client's `Authorization: Bearer` is its own NetBox token (NetBox enforces its permissions) |
| `NETBOX_MCP_ALLOWED_HOSTS`, `NETBOX_TIMEOUT` | | Extra `Host` values behind a proxy; request timeout (30 s) |

## Tools

**Read** (same names and parameters as upstream where they existed): `netbox_get_objects`, `netbox_get_object_by_id`, `netbox_get_changelogs`, `netbox_search_objects`. New: `netbox_list_object_types`, `netbox_describe_type` (fields, required, choices, filters, extra actions from the instance's own OpenAPI), `netbox_get_status`, `netbox_api_get`, `netbox_graphql` (queries only), `netbox_trace`, `netbox_get_available`, `netbox_rack_elevation`, `netbox_render_config`.

**Write**: `netbox_create_object`, `netbox_bulk_create`, `netbox_update_object` (returns `previous` to revert), `netbox_bulk_update`, `netbox_allocate` (next free IP / prefix / VLAN / ASN, race-free), `netbox_api_write`.

**Destructive**: `netbox_delete_object` and `netbox_bulk_delete` (both return a snapshot of what was removed), `netbox_run_script` (`commit=false` by default), `netbox_api_delete`.

Every write tool also takes `dry_run`.

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

## Claude Code skills

| Skill | File | Use |
|---|---|---|
| `nbmcp` | `.claude/skills/nbmcp/SKILL.md` | Using the Rust server safely: tools, write switches, confirmation, plugins |
| `netbox-mcp-testing` | `.claude/skills/netbox-mcp-testing/SKILL.md` | Upstream's protocol for testing the Python server against a live NetBox (kept as is) |

## Tests

`cargo test`: unit tests per module plus `tests/e2e.rs`, which drives the server over JSON-RPC against a fake NetBox that records every request (read-only by default, redaction, write flow with `previous`, delete snapshot, dry-run sends nothing, type policy blocks before any request, bulk and allocate paths, changelog fallback, plugin discovery, GraphQL mutation refused).
Not tested: a real NetBox instance (the fake follows the documented API, but only a run against NetBox proves it).
