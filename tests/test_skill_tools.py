"""The portable skill must list exactly the tools the Rust server defines (none missing, none phantom)."""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def test_skill_lists_exactly_the_server_tools():
    # Same source as `nbmcp --list-tools`: the read( / write( entries of all_tools().
    src = (ROOT / "rust/src/tools.rs").read_text(encoding="utf-8")
    server = set(re.findall(r'\b(?:read|write)\(\s*"(netbox_\w+)"', src))
    skill = (ROOT / "skill/netbox-mcp-rust/SKILL.md").read_text(encoding="utf-8")
    documented = set(re.findall(r"^\| `(netbox_\w+)` \|", skill, re.M))
    assert server, "no tool found in tools.rs"
    assert not server - documented, f"missing from SKILL.md: {sorted(server - documented)}"
    assert not documented - server, f"phantom in SKILL.md: {sorted(documented - server)}"
    assert f"## Tools ({len(server)})" in skill, "tool count in the SKILL.md heading is stale"


def test_every_tool_has_a_test_mode_call():
    src = (ROOT / "rust/src/tools.rs").read_text(encoding="utf-8")
    server = set(re.findall(r'\b(?:read|write)\(\s*"(netbox_\w+)"', src))
    calls = set(
        json.loads((ROOT / "rust/test/fixtures/calls.json").read_text(encoding="utf-8"))
    ) - {"_doc"}
    assert calls == server, f"calls.json differs: {sorted(calls ^ server)}"


def test_agents_md_and_readme_counts_follow_the_tool_list():
    src = (ROOT / "rust/src/tools.rs").read_text(encoding="utf-8")
    n = len(set(re.findall(r'\b(?:read|write)\(\s*"(netbox_\w+)"', src)))
    for name in ("AGENTS.md", "README.md"):
        text = (ROOT / name).read_text(encoding="utf-8")
        assert f"all {n} tools" in text, f"{name}: tool count differs from tools.rs ({n})"


def test_skill_documents_the_confirmation_settings_of_the_server():
    # Every operator setting that changes how a destructive call is confirmed must be in the skill.
    cfg = (ROOT / "rust/src/config.rs").read_text(encoding="utf-8")
    skill = (ROOT / "skill/netbox-mcp-rust/SKILL.md").read_text(encoding="utf-8")
    for var in (
        "NETBOX_ELICIT",
        "NETBOX_CONFIRM_ARG",
        "NETBOX_APPROVAL_DIR",
        "MCP_TEST_APPROVER",
    ):
        assert var in cfg, f"{var} no longer read by config.rs: update this test"
        assert var in skill, f"{var} is not documented in SKILL.md"
