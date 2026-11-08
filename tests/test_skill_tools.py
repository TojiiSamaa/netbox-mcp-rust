"""The portable skill must list exactly the tools the Rust server defines (none missing, none phantom)."""

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
