# AGENTS.md

This repo is `nbmcp`, a NetBox MCP server in Rust (`rust/`; the upstream Python server is in `src/`).

To use its `netbox_*` tools, read [`skill/netbox-mcp-rust/SKILL.md`](skill/netbox-mcp-rust/SKILL.md): all 23 tools, what is possible and impossible (read-only by default), recipes and common errors. After adding or removing a tool in `rust/src/tools.rs`, update that file; `uv run pytest tests/test_skill_tools.py` fails if they differ.
