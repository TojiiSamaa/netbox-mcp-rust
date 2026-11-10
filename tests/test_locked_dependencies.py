"""uv.lock must not pin a release that already has a fix: the Docker image installs exactly this lock."""

import tomllib
from pathlib import Path

from packaging.version import Version

LOCK = Path(__file__).resolve().parent.parent / "uv.lock"

# package -> (first fixed release, advisories Trivy reported on the image on 2026-11-08).
# The "Container Scan" workflow fails on any CRITICAL/HIGH finding that has a fix, so a lock below
# these floors breaks CI on every pull request that touches a .py file. Raise a floor here when a
# new advisory is fixed upstream; never lower one.
FIRST_FIXED = {
    "authlib": ("1.9.1", "CVE-2026-96760 CVE-2026-114615 CVE-2026-114616 CVE-2026-116672"),
    "certifi": ("2026.10.18", "GHSA-6q48-6c7w-9gw2"),
    "dnspython": ("2.9.0", "CVE-2026-114605"),
    "fastmcp": ("3.4.10", "GHSA-7xgj-8392-5q4r"),
    "fastmcp-slim": ("3.4.10", "GHSA-7xgj-8392-5q4r"),
    "joserfc": ("1.7.6", "CVE-2026-119284"),
}


def test_lock_pins_no_release_with_a_known_fix():
    packages = tomllib.loads(LOCK.read_text(encoding="utf-8"))["package"]
    locked = {p["name"]: Version(p["version"]) for p in packages}
    too_old = [
        f"{name} {locked[name]} < {fixed} ({advisories})"
        for name, (fixed, advisories) in FIRST_FIXED.items()
        if name in locked and locked[name] < Version(fixed)
    ]
    assert not too_old, "uv.lock pins vulnerable releases:\n" + "\n".join(too_old)
