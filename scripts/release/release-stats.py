#!/usr/bin/env python3
"""release-stats.py — code vs. test vs. comment line-change table for a release's diff.

Buckets the diff between two git refs into the workspace's four areas (the
shared `rust/` workspace plus the three `apps/<platform>/` hosts — see
AGENTS.md's "Workspace layout"), and within each bucket splits lines into
"code" vs. "test": a changed line counts as test if it falls at/after the
file's `#[cfg(test)]` module boundary, or the whole file's path has a
component ending in `test`/`tests` case-insensitively (`tests/`, `Tests/`,
`winui.tests/`, `*Tests.swift`/`.cs` — covers this repo's Rust integration
tests, macOS XCTest target, and Windows xunit project). Full-line `//`
comments are tallied separately in their own "comments" column, regardless
of whether they fall in code or test regions.

Also reports the **average published-binary size delta** (GitHub Release
platform packages only — not install scripts or checksums) as a fifth column
on the Total row: mean size of the six host archives for `new-ref` minus the
mean for `old-ref` (via `gh release view`).

Used by the `make-release` skill to generate the change-stats table appended
to release notes. Run from the repo root:

    scripts/release/release-stats.py v0.2.0 v0.2.1
"""
from __future__ import annotations

import json
import re
import subprocess
import sys

BUCKETS = [
    ("rust", ["rust"]),
    ("apps/macos", ["apps/macos"]),
    ("apps/windows", ["apps/windows"]),
    ("apps/linux", ["apps/linux"]),
]

HUNK_RE = re.compile(
    r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@.*\n((?:(?!@@).*\n?)*)",
    re.MULTILINE,
)
TEST_MOD_RE = re.compile(r"^\s*#\[cfg\(test\)\]")
TEST_PATH_RE = re.compile(r"(^|/)[^/]*[Tt]ests?(/|$)")

# Platform packages only (names from release.yml publish). Not install.* / checksums.
BINARY_ASSET_RE = re.compile(
    r"^dontspeak-.+-(linux-(x86_64|aarch64)|macos-(x86_64|aarch64)|windows-(x86_64|aarch64))\."
    r"(tar\.gz|app\.zip|zip)$"
)


def sh(cmd):
    return subprocess.run(
        cmd, capture_output=True, text=True, errors="replace", check=True
    ).stdout


def is_comment(line):
    return line.strip().startswith("//")


def test_module_boundary(rev, path):
    """1-indexed line where a `#[cfg(test)]` module starts, or None."""
    result = subprocess.run(
        ["git", "show", f"{rev}:{path}"],
        capture_output=True,
        text=True,
        errors="replace",
    )
    if result.returncode != 0:
        return None
    for lineno, line in enumerate(result.stdout.split("\n"), start=1):
        if TEST_MOD_RE.match(line):
            return lineno
    return None


def bucket_stats(old_ref, new_ref, paths):
    files = [
        f
        for f in sh(["git", "diff", "--name-only", f"{old_ref}..{new_ref}", "--", *paths]).splitlines()
        if f
    ]
    code_add = code_del = test_add = test_del = comment_add = comment_del = 0
    for path in files:
        whole_file_is_test = bool(TEST_PATH_RE.search(path))
        old_boundary = None if whole_file_is_test else test_module_boundary(old_ref, path)
        new_boundary = None if whole_file_is_test else test_module_boundary(new_ref, path)
        diff = sh(["git", "diff", "--unified=0", f"{old_ref}..{new_ref}", "--", path])
        for hunk in HUNK_RE.finditer(diff):
            old_start, new_start = int(hunk.group(1)), int(hunk.group(3))
            body_lines = hunk.group(5).split("\n")
            removed = [l[1:] for l in body_lines if l.startswith("-")]
            added = [l[1:] for l in body_lines if l.startswith("+")]
            for i, line in enumerate(removed):
                if is_comment(line):
                    comment_del += 1
                    continue
                is_test = whole_file_is_test or (
                    old_boundary is not None and old_start + i >= old_boundary
                )
                test_del += is_test
                code_del += not is_test
            for i, line in enumerate(added):
                if is_comment(line):
                    comment_add += 1
                    continue
                is_test = whole_file_is_test or (
                    new_boundary is not None and new_start + i >= new_boundary
                )
                test_add += is_test
                code_add += not is_test
    return code_add, code_del, test_add, test_del, comment_add, comment_del


def fmt(added, removed):
    return f"+{added} / -{removed}"


def release_binary_sizes(tag: str) -> dict[str, int] | None:
    """Map platform package name → size bytes from `gh release view`, or None."""
    result = subprocess.run(
        ["gh", "release", "view", tag, "--json", "assets"],
        capture_output=True,
        text=True,
        errors="replace",
    )
    if result.returncode != 0:
        return None
    try:
        assets = json.loads(result.stdout).get("assets") or []
    except json.JSONDecodeError:
        return None
    out: dict[str, int] = {}
    for a in assets:
        name = a.get("name") or ""
        if BINARY_ASSET_RE.match(name):
            out[name] = int(a.get("size") or 0)
    return out or None


def average_binary_size(sizes: dict[str, int]) -> float:
    return sum(sizes.values()) / len(sizes)


def fmt_size_bump(delta_bytes: float) -> str:
    """Human size delta for the table (KiB if |Δ|<1 MiB, else MiB)."""
    sign = "+" if delta_bytes >= 0 else "-"
    abs_b = abs(delta_bytes)
    if abs_b < 1024 * 1024:
        return f"{sign}{abs_b / 1024:.0f} KiB"
    return f"{sign}{abs_b / (1024 * 1024):.2f} MiB"


def size_bump_cell(old_ref: str, new_ref: str) -> str:
    """Fifth-column value for Total, or em-dash if assets are missing."""
    old_sizes = release_binary_sizes(old_ref)
    new_sizes = release_binary_sizes(new_ref)
    if not old_sizes or not new_sizes:
        return "—"
    # Average only platforms present in both releases (name suffix after version).
    def platform_key(name: str) -> str | None:
        # dontspeak-<ver>-<platform>.<ext>
        m = re.search(
            r"-(linux-(?:x86_64|aarch64)|macos-(?:x86_64|aarch64)|windows-(?:x86_64|aarch64))\.",
            name,
        )
        return m.group(1) if m else None

    old_by_plat = {platform_key(n): s for n, s in old_sizes.items() if platform_key(n)}
    new_by_plat = {platform_key(n): s for n, s in new_sizes.items() if platform_key(n)}
    common = sorted(set(old_by_plat) & set(new_by_plat))
    if not common:
        return "—"
    old_avg = sum(old_by_plat[p] for p in common) / len(common)
    new_avg = sum(new_by_plat[p] for p in common) / len(common)
    detail = f"{len(common)} pkg avg"
    return f"{fmt_size_bump(new_avg - old_avg)} ({detail})"


def main():
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <old-ref> <new-ref>", file=sys.stderr)
        sys.exit(1)
    old_ref, new_ref = sys.argv[1], sys.argv[2]

    rows = []
    totals = [0, 0, 0, 0, 0, 0]
    for label, paths in BUCKETS:
        stats = bucket_stats(old_ref, new_ref, paths)
        rows.append((label, stats))
        totals = [t + s for t, s in zip(totals, stats)]

    size_cell = size_bump_cell(old_ref, new_ref)

    print("| Area | Code | Tests | Comments | Size bump (avg bin) |")
    print("|---|---:|---:|---:|---:|")
    for label, (code_add, code_del, test_add, test_del, comment_add, comment_del) in rows:
        print(
            f"| `{label}` | {fmt(code_add, code_del)} | {fmt(test_add, test_del)} | "
            f"{fmt(comment_add, comment_del)} | |"
        )
    print(
        f"| **Total** | **{fmt(totals[0], totals[1])}** | **{fmt(totals[2], totals[3])}** "
        f"| **{fmt(totals[4], totals[5])}** | **{size_cell}** |"
    )


if __name__ == "__main__":
    main()
