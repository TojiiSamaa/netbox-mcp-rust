# Changelog — nbmcp (Rust server)

Changes to the Rust server in `rust/` (binary `nbmcp`). The upstream Python server keeps its own generated changelog in [`../CHANGELOG.md`](../CHANGELOG.md), which this file does not touch.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning: [SemVer](https://semver.org/); the version in `Cargo.toml` is the one the binary reports and the one the Git tag and Docker image carry.

## [Unreleased]

### Changed
- Repository tooling: the new-issues hook is now started from `$CLAUDE_PROJECT_DIR`, so it also runs when the session's working directory is a subfolder such as `rust/` (a relative path made it fail silently there).
- CI: the release workflow file is renamed `.github/workflows/release-rust.yml` (was `rust-release.yml`). No change to what it does.

### Open
- issue #4: waiting for the reporter to replay the reproduction against a binary at or after 0.2.0.

## [0.2.1] — 2026-11-08 <!-- scan:ok -->

Fixes found by a first full pass of the 23 tools against a throw-away NetBox 4.7.4 in dry-run mode.

### Fixed
- **`netbox_describe_type` returned `fields: []`** (bug NETBOX-2611080739-2et). NetBox 4 declares the list POST body as `oneOf: [object, array of objects]` (bulk create), and the reader looked for `properties` at the root. It now walks `$ref`, `allOf`, `oneOf` and `anyOf`, ignores the array member, merges `required`, and falls back to the PATCH body of the detail endpoint. New unit test with the `oneOf` shape. `actions: []` is correct for types that have no extra endpoint, such as `dcim.site`.
- **Dry-run of `netbox_update_object`, `netbox_delete_object`, `netbox_bulk_update` and `netbox_bulk_delete` failed with 404 when an id did not exist** and showed no simulated request (bug NETBOX-2611080739-ox4). In dry-run, a missing object no longer stops the preview: the answer carries `exists` (or `found` for bulk), a `warning` that a real call would fail with 404, and `would_send` holds the request that would be sent. A real call still fails with 404 before sending anything.
- **Reading an object that does not exist is now an answer, not a tool error** (bug NETBOX-2611080739-ca3). Read tools return `{"found": false, "error": {...}}` (no `isError`) for a JSON 404; an unknown address (HTML 404) is still an error. The server itself has no circuit breaker (checked in the source): the ~59 s pause seen after three 404s comes from the MCP client counting each `isError` answer as a failure. This change stops those lookups from tripping it; if the client keeps a breaker, its threshold is the client's setting.

- **`confirm` was missing from the published `inputSchema`** (issue #5). The schema is closed (`additionalProperties: false`), so a client that validates its arguments could not send the `confirm: true` that `confirmation_required` asks for, although the server accepted it. With `NETBOX_CONFIRM_ARG=true`, every tool that needs a confirmation under the current `NETBOX_ELICIT` now lists an optional boolean `confirm` ("send true only after the user has explicitly agreed to this exact action"). Nothing is added when the fallback is off or the tool needs no confirmation. Test: `confirm_is_published_in_the_schema_when_the_fallback_is_on`.

### Added
- Tests proving the dry-run guarantee: with `NETBOX_DRY_RUN=true`, ten write and destructive tools called with `dry_run: false` all answer `dry_run: true` with a non-empty `would_send`, and the fake NetBox receives **zero** non-GET requests (`dry_run_env_forces_the_simulation_even_when_the_call_says_dry_run_false`). Also `dry_run_previews_updates_and_deletes_of_missing_objects` and `reading_a_missing_object_is_an_answer_not_a_tool_error`. Totals: 32 unit + 19 end-to-end, `cargo clippy --all-targets` clean.

### Changed
- Dry-run answers of the four tools above gain `exists` / `found` and `warning` fields; `netbox_update_object` in dry-run no longer returns only `current`.
- Lookups of a missing object by id, trace, rack elevation, render-config and available-* now answer with `found: false` instead of `isError: true`.

### Not done (reported as gaps, to be decided)
CSV import/export, server-side validation before a write (OPTIONS or `commit=false`), cables, assigning an IP to an interface or VM in one call, tags and journal entries, moving a device between racks.

## [0.2.0] — 2026-11-08 <!-- scan:ok -->

Safety release: destructive calls can no longer slip through when the server has no way to ask a human. Also adds the Docker image and the release chain.

### Security
- **Destructive calls now fail closed** (issue #1, commit `12221e9`). Before, with `NETBOX_ELICIT=destructive` (the default) the server silently accepted a delete / bulk delete / script run / raw DELETE when the client could not be asked for confirmation (HTTP transport, or a client without the MCP `elicitation` capability). It now refuses with the new typed error `confirmation_required`, **before any request reaches NetBox**.
- `NETBOX_ELICIT=off` is the explicit way to accept unconfirmed calls (what the end-to-end delete test now uses).
- **HTTP `Host` check no longer breaks reverse proxies and containers** (commit `bbbb6f9`). The DNS-rebinding protection (`Host` allow-list) is enforced only on loopback binds, or when `NETBOX_MCP_ALLOWED_HOSTS` is set. Behind `-p 8000:8000` or a proxy, requests used to get `403 Host not allowed`.
- Binding outside loopback still requires `NETBOX_MCP_TOKEN` (or `MCP_AUTH_TOKEN`) or `NETBOX_TOKEN_PASSTHROUGH=true`; the server refuses to start otherwise.

### Added
- `NETBOX_CONFIRM_ARG` (default `false`): opt-in fallback for clients that cannot be asked (HTTP, no elicitation). When on, a destructive call is accepted if it carries `confirm: true`. The refusal message tells the model to ask the user first and only then retry with `confirm: true`. Off by default, so the safe behaviour needs no configuration (commit `7961354`, follow-up to #1 after issue #4 showed the HTTP case).
- `confirm` boolean argument on every non-read tool (next to `dry_run`), stripped before the request is built; ignored unless `NETBOX_CONFIRM_ARG=true`.
- `confirmation_required` error type, with a hint adapted to whether `NETBOX_CONFIRM_ARG` is on.
- **Docker image**: `rust/Dockerfile` (multi-stage `rust:1-alpine` to `alpine:3`, non-root user uid 1000, entrypoint `nbmcp`, default `--transport http --host 0.0.0.0 --port 8000`) and `rust/.dockerignore`.
- **Release chain** in `.github/workflows/release-rust.yml`, one run on a `vX.Y.Z` tag or a manual dispatch. It checks that the version equals the one in `rust/Cargo.toml`, builds linux/amd64 and linux/arm64 natively, pushes both images to the GitHub container registry, assembles the multi-arch manifest (tags `X.Y.Z`, `X.Y`, `X` and `latest`; pre-releases get only the full version) with the source annotation that links the package to the repository, then creates the GitHub Release with the `docker pull` command in its notes. Image and Release come from the same run, so they always match.
- Upstream's Docker Hub workflow (`docker-publish.yml`) is restricted to `netboxlabs/netbox-mcp-server`, so it no longer runs on the fork.
- Tests: three end-to-end tests: destructive calls fail closed with no way to confirm, `confirm: true` is an opt-in fallback only, and elicitation accept / refuse when the client supports it. Totals: 31 unit + 15 end-to-end, `cargo clippy --all-targets` clean.
- Docs: README sections for the new variables, Docker and Claude Code skills; `.claude/skills/nbmcp/SKILL.md` (how to use this server safely); `CONTEXTE-AGENT.md` (context for agents working on the repo); this changelog.
- Repository tooling: `.claude/hooks/new-issues.sh` reports newly opened issues at session start and on each prompt (polling, not a push notification).

### Fixed
- Confirmation could be skipped on HTTP and on clients without elicitation (see Security).
- `403 Host not allowed` when the server ran in a container or behind a proxy (see Security).

### Known limits
- Never run against a real NetBox instance: the end-to-end tests use a fake that follows the documented API and records every request.
- The release workflow had not run on GitHub when this version was prepared (arm64 build, GHCR publication and package-to-repository link unverified).

## [0.1.0] — 2026-11-08 <!-- scan:ok -->

First release: Rust port of [`netboxlabs/netbox-mcp-server`](https://github.com/netboxlabs/netbox-mcp-server) (commit `6f0dbb7`), with the write side upstream deliberately left out and plugin support. One static binary, no Python runtime. The original Python server stays in `src/` untouched (README moved to `README-python.md`).

### Added — server
- Hand-rolled MCP server: JSON-RPC 2.0 over **stdio** and over **stateless streamable HTTP** (axum, endpoint `/mcp`).
- Configuration order: CLI flags, then environment, then `.env` in the current directory. Upstream variable names kept (`NETBOX_URL`, `NETBOX_TOKEN`, `VERIFY_SSL`, `LOG_LEVEL`, `CORS_ORIGINS`, `MCP_AUTH_TOKEN`, `ENABLE_PLUGIN_DISCOVERY`).
- `--list-tools`: prints the tools visible with the current configuration.
- Token handling: `nbt_…` v2 tokens sent as `Bearer`, v1 tokens as `Token`; optional per-request token passthrough on HTTP (`NETBOX_TOKEN_PASSTHROUGH`) so NetBox enforces the caller's own permissions.
- Optional HTTP bearer protection (`NETBOX_MCP_TOKEN` / `MCP_AUTH_TOKEN`), CORS (`CORS_ORIGINS`), `Host` allow-list (`NETBOX_MCP_ALLOWED_HOSTS`), request timeout (`NETBOX_TIMEOUT`, default 30 s).
- Redirects are never followed (a redirected write would silently lose its body).
- Server instructions tell the model that anything NetBox returns is untrusted data.

### Added — 23 tools
Read (same names and parameters as upstream where they existed):
- `netbox_get_objects`, `netbox_get_object_by_id`, `netbox_get_changelogs`, `netbox_search_objects`.
- New read tools: `netbox_list_object_types`, `netbox_describe_type`, `netbox_get_status`, `netbox_api_get`, `netbox_graphql` (queries only), `netbox_trace`, `netbox_get_available`, `netbox_rack_elevation`, `netbox_render_config`. `netbox_describe_type` reads fields, required flags, choices, filters and extra actions from the instance OpenAPI.

Write:
- `netbox_create_object`, `netbox_bulk_create`, `netbox_update_object` (returns `previous` so the change can be reverted), `netbox_bulk_update`, `netbox_allocate` (next free IP / prefix / VLAN / ASN, race-free), `netbox_api_write` (raw POST / PUT / PATCH).

Destructive:
- `netbox_delete_object` and `netbox_bulk_delete` (both return a snapshot of what was removed), `netbox_run_script` (`commit=false` by default), `netbox_api_delete`.

Every write tool also takes `dry_run`.

### Added — plugins
- Plugin models are first-class: with discovery on (`ENABLE_PLUGIN_DISCOVERY`, default true) their types are learned at startup from `core/object-types` and every typed tool works on them (for example `netbox_dns.zone`).
- `netbox_api_get` / `netbox_api_write` / `netbox_api_delete` reach any path under `/api`, under the same write switches and type policy (a path is mapped back to its type; unknown paths are refused when `NETBOX_WRITE_TYPES` is restrictive).
- Built-in type table in `rust/types.json`.

### Added — safety model
Writes off, then a separate delete switch, then type allow/deny, then optional human confirmation, then dry-run, then audit log. A refusal happens before any request is made; NetBox's own token permissions still apply on top.
- `NETBOX_ALLOW_WRITES` (default off): create / update / allocate / raw POST-PUT-PATCH.
- `NETBOX_ALLOW_DELETE` (default off, needs `ALLOW_WRITES`): delete, bulk delete, scripts, raw DELETE.
- `NETBOX_DRY_RUN`: every write is previewed (`would_send`) and nothing is sent; write tools are listed even when writes are off.
- `NETBOX_WRITE_TYPES` (default `*`) and `NETBOX_WRITE_DENY`: glob patterns such as `dcim.*,netbox_dns.zone`. `*` never matches `users.*`.
- `NETBOX_ELICIT` (`off` / `destructive` / `all`, default `destructive`): ask the human through MCP elicitation before acting.
- `NETBOX_MAX_BULK` (default 100) and `NETBOX_MAX_LIMIT` (default 100): items per bulk call and page size.
- `NETBOX_REDACT_SECRETS` (default on): masks `preshared_key`, `psk`, `password`, `secret`… and token keys for `users.token`.
- `NETBOX_AUDIT_LOG`: JSONL file, one line per write attempt (tool, arguments, outcome, requests sent).
- `NETBOX_DISABLED_TOOLS`: comma list of tools to hide.
- `NETBOX_MAX_RESPONSE_CHARS` (default 60000): answers are cut by rows with `next_offset`, never mid-JSON.

### Fixed — differences from upstream
- Bulk operations use NetBox's real API (POST / PATCH / DELETE of a list on the list endpoint); upstream's `/bulk/` path does not exist.
- `netbox_search_objects`: a type that fails is reported in `errors`; if the token is refused for every type the call fails instead of answering "no results".
- `netbox_get_changelogs`: falls back to `extras/object-changes` on NetBox older than 4.1; default `limit` 10 (entries carry full before/after data).
- Filters: `__in` and multi-hop lookups (`device__site_id`) are rejected with an explanation instead of silently returning wrong data; list values become repeated query parameters.
- HTTP 4xx / 5xx become typed errors with a hint and NetBox's per-field details; a JSON 404 (missing object) is told apart from an HTML 404 (unknown endpoint).
- Responses over the size limit are cut by row with `next_offset`.

### Known limits
- Never run against a real NetBox instance (see 0.2.0).
- Destructive confirmation had the fail-open flaw fixed in 0.2.0; do not use 0.1.0 over HTTP with writes and delete enabled.

