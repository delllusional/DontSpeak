#!/usr/bin/env python3
"""release-stats.py — code vs. test line-change table for a release's diff.

Buckets the diff between two git refs into the workspace's four areas (the
shared `rust/` workspace plus the three `apps/<platform>/` hosts — see
AGENTS.md's "Workspace layout"), and within each bucket splits lines into
"code" vs. "test": a changed line counts as test if it falls at/after the
file's `#[cfg(test)]` module boundary, or the whole file's path has a
component ending in `test`/`tests` case-insensitively (`tests/`, `Tests/`,
`winui.tests/`, `*Tests.swift`/`.cs` — covers this repo's Rust integration
tests, macOS XCTest target, and Windows xunit project). Full-line `//`
comments are excluded from both counts.

Used by the `make-release` skill's step 6 (write real release notes) to
generate the change-stats table appended there. Run from the repo root:

    scripts/release-stats.py v0.2.0 v0.2.1
"""
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
    code_add = code_del = test_add = test_del = 0
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
                    continue
                is_test = whole_file_is_test or (old_boundary is not None and old_start + i >= old_boundary)
                test_del += is_test
                code_del += not is_test
            for i, line in enumerate(added):
                if is_comment(line):
                    continue
                is_test = whole_file_is_test or (new_boundary is not None and new_start + i >= new_boundary)
                test_add += is_test
                code_add += not is_test
    return code_add, code_del, test_add, test_del


def fmt(added, removed):
    return f"+{added} / -{removed}"


def main():
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <old-ref> <new-ref>", file=sys.stderr)
        sys.exit(1)
    old_ref, new_ref = sys.argv[1], sys.argv[2]

    rows = []
    totals = [0, 0, 0, 0]
    for label, paths in BUCKETS:
        stats = bucket_stats(old_ref, new_ref, paths)
        rows.append((label, stats))
        totals = [t + s for t, s in zip(totals, stats)]

    print("| Area | Code | Tests |")
    print("|---|---:|---:|")
    for label, (code_add, code_del, test_add, test_del) in rows:
        print(f"| `{label}` | {fmt(code_add, code_del)} | {fmt(test_add, test_del)} |")
    print(f"| **Total** | **{fmt(totals[0], totals[1])}** | **{fmt(totals[2], totals[3])}** |")


if __name__ == "__main__":
    main()
