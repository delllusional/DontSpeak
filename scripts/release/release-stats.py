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

Also reports **Binaries** size deltas (GitHub Release host packages only — not
install scripts or checksums): each host app row averages that OS's two arch
packages; **Total** averages all six; `rust` is blank. At tag time the new
release's assets don't exist yet, so the default mode writes `…` placeholders in
that column; after publish, release.yml reruns this script in `--patch-sizes`
mode, which rewrites ONLY those cells from the just-built artifacts on disk vs
the previous release's published sizes, leaving the tag annotation untouched.

Used by the `make-release` skill to generate the change-stats table appended
to release notes. Run from the repo root:

    scripts/release/release-stats.py v0.2.0 HEAD
    scripts/release/release-stats.py --patch-sizes body.md --old v0.2.0 --assets-dir artifacts
"""
from __future__ import annotations

import argparse
import json
import os
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


def fmt_size_bump(delta_bytes: float) -> str:
    """Human size delta for the table (KiB if |Δ|<1 MiB, else MiB)."""
    sign = "+" if delta_bytes >= 0 else "-"
    abs_b = abs(delta_bytes)
    if abs_b < 1024 * 1024:
        return f"{sign}{abs_b / 1024:.0f} KiB"
    return f"{sign}{abs_b / (1024 * 1024):.2f} MiB"


def platform_key(name: str) -> str | None:
    """e.g. dontspeak-0.3.2-linux-x86_64.tar.gz → linux-x86_64."""
    m = re.search(
        r"-(linux-(?:x86_64|aarch64)|macos-(?:x86_64|aarch64)|windows-(?:x86_64|aarch64))\.",
        name,
    )
    return m.group(1) if m else None


# Area row → which platform package keys feed that row's average.
# `rust` is empty: shared code is not a published host package by itself.
AREA_PLATFORMS = {
    "rust": (),
    "apps/macos": ("macos-x86_64", "macos-aarch64"),
    "apps/windows": ("windows-x86_64", "windows-aarch64"),
    "apps/linux": ("linux-x86_64", "linux-aarch64"),
}


ALL_PLATFORMS = (
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
    "windows-aarch64",
)


def sizes_by_platform_from_release(tag: str) -> dict[str, int] | None:
    """Platform key → size bytes for a published release, or None (gh failure / no assets)."""
    sizes = release_binary_sizes(tag)
    if sizes is None:
        return None
    return {platform_key(n): s for n, s in sizes.items() if platform_key(n)}


def sizes_by_platform_from_dir(d: str) -> dict[str, int]:
    """Platform key → size bytes for packages under `d` (recursive — handles the
    release job's nested `artifacts/<artifact-name>/` layout). Basenames must match
    BINARY_ASSET_RE; anything else (installers, checksums) is ignored."""
    out: dict[str, int] = {}
    for root, _dirs, files in os.walk(d):
        for name in files:
            if BINARY_ASSET_RE.match(name):
                key = platform_key(name)
                if key:
                    out[key] = os.path.getsize(os.path.join(root, name))
    return out


def size_cells(
    old_by: dict[str, int] | None, new_by: dict[str, int] | None
) -> dict[str, str]:
    """Map area label (and 'Total') → fifth-column cell text.

    Host app rows: mean size of that OS's packages (both arches when present in
    both maps). Total: mean of all six packages. rust: blank. No new sizes (the
    pre-tag run: the release doesn't exist yet) → `…` placeholders for CI's
    post-publish --patch-sizes pass. No old sizes with real new ones (first-ever
    release) → `—` via the no-common-platforms path.
    """
    cells = {label: "" for label, _ in BUCKETS}
    if not new_by:
        for label, plats in AREA_PLATFORMS.items():
            if plats:
                cells[label] = "…"
        cells["Total"] = "…"
        return cells
    old_by = old_by or {}

    def avg_delta(plats: tuple[str, ...]) -> str:
        common = [p for p in plats if p in old_by and p in new_by]
        if not common:
            return "—"
        old_avg = sum(old_by[p] for p in common) / len(common)
        new_avg = sum(new_by[p] for p in common) / len(common)
        return fmt_size_bump(new_avg - old_avg)

    for label, plats in AREA_PLATFORMS.items():
        if plats:  # rust: no host package, stays blank
            cells[label] = avg_delta(plats)
    cells["Total"] = avg_delta(ALL_PLATFORMS)
    return cells


LINES_HEADER_ROW = "| Area | Code | Tests | Comments | Binaries avg |"


def patch_lines_table(text: str, cells: dict[str, str]) -> str | None:
    """Rewrite the fifth column of the first Lines table in `text`; None if absent.

    Overwrites area/Total rows' Binaries-avg cells unconditionally (placeholder,
    blank, or stale) with `cells` values, bolding Total to match the generator.
    Preserves everything else byte-for-byte, including CRLF vs LF line endings.
    """
    lines = text.splitlines(keepends=True)
    labels = {label for label, _ in BUCKETS} | {"Total"}
    header_idx = None
    for i, line in enumerate(lines):
        if line.rstrip("\r\n") == LINES_HEADER_ROW:
            header_idx = i
            break
    if header_idx is None:
        return None
    for i in range(header_idx + 1, len(lines)):
        body = lines[i].rstrip("\r\n")
        if not body.startswith("|"):
            break  # contiguous table rows only
        ending = lines[i][len(body):]
        parts = body.split("|")
        if len(parts) != 7:  # not a well-formed 5-column row
            continue
        label = parts[1].strip().strip("`*")
        if label not in labels:
            continue
        value = cells.get(label, "")
        parts[5] = f" **{value}** " if label == "Total" else f" {value} "
        lines[i] = "|".join(parts) + ending
    return "".join(lines)


def patch_sizes(body_path: str, old_tag: str, assets_dir: str) -> None:
    """--patch-sizes mode: fill the body file's Binaries-avg cells in place."""
    with open(body_path, newline="", encoding="utf-8") as f:
        text = f.read()
    old_by = sizes_by_platform_from_release(old_tag)
    if old_by is None:
        # Transient gh hiccup must not overwrite placeholders with bogus `—`.
        print(
            f"error: no published sizes for {old_tag} (gh failed?) — {body_path} untouched",
            file=sys.stderr,
        )
        sys.exit(1)
    new_by = sizes_by_platform_from_dir(assets_dir)
    patched = patch_lines_table(text, size_cells(old_by, new_by))
    if patched is None:
        print(f"no Lines table in {body_path} — nothing to patch")
        return
    with open(body_path, "w", newline="", encoding="utf-8") as f:
        f.write(patched)
    # ASCII-only status: Windows consoles may still be cp1252.
    print(f"patched Binaries avg cells in {body_path} ({old_tag} vs {assets_dir})")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("refs", nargs="*", metavar="ref", help="<old-ref> <new-ref>")
    ap.add_argument(
        "--patch-sizes",
        metavar="BODY_FILE",
        help="rewrite the Binaries-avg cells of BODY_FILE's Lines table in place",
    )
    ap.add_argument("--old", metavar="PREV_TAG", help="previous published release tag")
    ap.add_argument(
        "--assets-dir", metavar="DIR", help="directory holding the new release's packages"
    )
    args = ap.parse_args()

    if args.patch_sizes:
        if not args.old or not args.assets_dir:
            ap.error("--patch-sizes requires --old and --assets-dir")
        patch_sizes(args.patch_sizes, args.old, args.assets_dir)
        return
    if args.old or args.assets_dir:
        ap.error("--old/--assets-dir only apply with --patch-sizes")
    if len(args.refs) != 2:
        ap.error("expected <old-ref> <new-ref>")
    old_ref, new_ref = args.refs

    rows = []
    totals = [0, 0, 0, 0, 0, 0]
    for label, paths in BUCKETS:
        stats = bucket_stats(old_ref, new_ref, paths)
        rows.append((label, stats))
        totals = [t + s for t, s in zip(totals, stats)]

    cells = size_cells(
        sizes_by_platform_from_release(old_ref), sizes_by_platform_from_release(new_ref)
    )

    print(LINES_HEADER_ROW)
    print("|---|---:|---:|---:|---:|")
    for label, (code_add, code_del, test_add, test_del, comment_add, comment_del) in rows:
        size = cells.get(label, "")
        print(
            f"| `{label}` | {fmt(code_add, code_del)} | {fmt(test_add, test_del)} | "
            f"{fmt(comment_add, comment_del)} | {size} |"
        )
    total_size = cells.get("Total", "—")
    print(
        f"| **Total** | **{fmt(totals[0], totals[1])}** | **{fmt(totals[2], totals[3])}** "
        f"| **{fmt(totals[4], totals[5])}** | **{total_size}** |"
    )


if __name__ == "__main__":
    main()
