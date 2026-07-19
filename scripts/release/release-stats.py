#!/usr/bin/env python3
"""release-stats.py — code/test/comment line-change table for a release diff.

Buckets two git refs into rust + apps/{macos,windows,linux}. A changed line is
"test" if it is at/after `#[cfg(test)]` or the path has a test/tests component
(case-insensitive). Full-line `//` comments are a separate column.

Binaries column: mean size delta of that OS's two arch packages (GH release
host packages only). Total and rust leave it blank. Pre-tag: `…` placeholders;
post-publish release.yml runs `--patch-sizes` to fill only those cells.

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
    """1-indexed line of `#[cfg(test)]`, or None."""
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
    """Package name → size from `gh release view`, or None."""
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
    """Size delta: KiB if |Δ|<1 MiB, else MiB."""
    sign = "+" if delta_bytes >= 0 else "-"
    abs_b = abs(delta_bytes)
    if abs_b < 1024 * 1024:
        return f"{sign}{abs_b / 1024:.0f} KiB"
    return f"{sign}{abs_b / (1024 * 1024):.2f} MiB"


def platform_key(name: str) -> str | None:
    """Asset basename → platform key (e.g. linux-x86_64)."""
    m = re.search(
        r"-(linux-(?:x86_64|aarch64)|macos-(?:x86_64|aarch64)|windows-(?:x86_64|aarch64))\.",
        name,
    )
    return m.group(1) if m else None


# Area row → package keys for that row's average. rust: no host package.
AREA_PLATFORMS = {
    "rust": (),
    "apps/macos": ("macos-x86_64", "macos-aarch64"),
    "apps/windows": ("windows-x86_64", "windows-aarch64"),
    "apps/linux": ("linux-x86_64", "linux-aarch64"),
}


def sizes_by_platform_from_release(tag: str) -> dict[str, int] | None:
    """Platform key → size for a published release, or None."""
    sizes = release_binary_sizes(tag)
    if sizes is None:
        return None
    return {platform_key(n): s for n, s in sizes.items() if platform_key(n)}


def sizes_by_platform_from_dir(d: str) -> dict[str, int]:
    """Platform key → size under `d` (recursive; BINARY_ASSET_RE basenames only)."""
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
    """Area/Total → fifth-column text.

    Host rows: mean of that OS's packages present in both maps. Total/rust blank.
    No new sizes → `…` (pre-tag). No common platforms → `—` (first release / partial).
    """
    cells = {label: "" for label, _ in BUCKETS}
    cells["Total"] = ""
    if not new_by:
        for label, plats in AREA_PLATFORMS.items():
            if plats:
                cells[label] = "…"
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
        if plats:
            cells[label] = avg_delta(plats)
    return cells


# GH markdown math so the overline average renders (ASCII "avg" did not).
LINES_HEADER_ROW = "| Area | Code | Tests | Comments | Binaries $\\overline{\\Delta}$ |"


def patch_lines_table(text: str, cells: dict[str, str]) -> str | None:
    """Rewrite fifth column of the first Lines table; None if absent.

    Overwrites size cells unconditionally. Preserves all other bytes (incl. CRLF).
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
            break
        ending = lines[i][len(body):]
        parts = body.split("|")
        if len(parts) != 7:
            continue
        label = parts[1].strip().strip("`*")
        if label not in labels:
            continue
        value = cells.get(label, "")
        if label == "Total":
            # Empty Total size cell: no bold (avoid `****`).
            parts[5] = f" **{value}** " if value else "  "
        else:
            parts[5] = f" {value} "
        lines[i] = "|".join(parts) + ending
    return "".join(lines)


def patch_sizes(body_path: str, old_tag: str, assets_dir: str) -> None:
    """Fill body file Binaries size cells in place (--patch-sizes)."""
    with open(body_path, newline="", encoding="utf-8") as f:
        text = f.read()
    old_by = sizes_by_platform_from_release(old_tag)
    if old_by is None:
        # gh hiccup must not overwrite placeholders with bogus `—`.
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
    # ASCII-only: Windows consoles may still be cp1252.
    print(f"patched Binaries size cells in {body_path} ({old_tag} vs {assets_dir})")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("refs", nargs="*", metavar="ref", help="<old-ref> <new-ref>")
    ap.add_argument(
        "--patch-sizes",
        metavar="BODY_FILE",
        help="rewrite the Binaries size cells of BODY_FILE's Lines table in place",
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
    print(
        f"| **Total** | **{fmt(totals[0], totals[1])}** | **{fmt(totals[2], totals[3])}** "
        f"| **{fmt(totals[4], totals[5])}** |  |"
    )


if __name__ == "__main__":
    main()
