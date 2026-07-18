#!/usr/bin/env python3
"""Tests for release-stats.py's pure size/patch logic (no gh, git, or network).

Guards the placeholder-then-CI-patch lifecycle: pre-tag runs must emit `…`
cells, and release.yml's --patch-sizes pass must rewrite ONLY the Binaries-avg
column of the Lines table — byte-identical everywhere else, either line ending.
"""
from __future__ import annotations

import importlib.util
import os
import tempfile
import unittest

# The module filename has a dash, so import it by path.
_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "release_stats", os.path.join(_HERE, "release-stats.py")
)
rs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rs)

KIB = 1024
MIB = 1024 * 1024

# Mirrors the release job's artifacts/<artifact-name>/<package> layout.
ARTIFACT_FILES = {
    "windows-portable-x64/dontspeak-0.3.2-windows-x86_64.zip": 10 * MIB,
    "windows-portable-arm64/dontspeak-0.3.2-windows-aarch64.zip": 8 * MIB,
    "macos-apps/dontspeak-0.3.2-macos-x86_64.app.zip": 6 * MIB,
    "macos-apps/dontspeak-0.3.2-macos-aarch64.app.zip": 4 * MIB,
    "linux-packages-x86_64/dontspeak-0.3.2-linux-x86_64.tar.gz": 3 * MIB,
    "linux-packages-aarch64/dontspeak-0.3.2-linux-aarch64.tar.gz": 1 * MIB,
}
DECOY_FILES = {
    "install.sh": 100,
    "checksums.txt": 200,
    "macos-apps/notes.md": 300,
}


def write_tree(root: str, files: dict[str, int]) -> None:
    for rel, size in files.items():
        path = os.path.join(root, *rel.split("/"))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(b"\0" * size)


class SizesFromDirTest(unittest.TestCase):
    def test_nested_layout_with_decoys(self):
        with tempfile.TemporaryDirectory() as d:
            write_tree(d, ARTIFACT_FILES)
            write_tree(d, DECOY_FILES)
            self.assertEqual(
                rs.sizes_by_platform_from_dir(d),
                {
                    "windows-x86_64": 10 * MIB,
                    "windows-aarch64": 8 * MIB,
                    "macos-x86_64": 6 * MIB,
                    "macos-aarch64": 4 * MIB,
                    "linux-x86_64": 3 * MIB,
                    "linux-aarch64": 1 * MIB,
                },
            )

    def test_missing_linux_yields_no_linux_keys(self):
        no_linux = {k: v for k, v in ARTIFACT_FILES.items() if "linux" not in k}
        with tempfile.TemporaryDirectory() as d:
            write_tree(d, no_linux)
            keys = set(rs.sizes_by_platform_from_dir(d))
            self.assertEqual(
                keys,
                {"windows-x86_64", "windows-aarch64", "macos-x86_64", "macos-aarch64"},
            )


FULL_OLD = {
    "windows-x86_64": 9 * MIB,
    "windows-aarch64": 7 * MIB,
    "macos-x86_64": 6 * MIB + 512 * KIB,
    "macos-aarch64": 4 * MIB + 512 * KIB,
    "linux-x86_64": 2 * MIB,
    "linux-aarch64": 2 * MIB,
}
FULL_NEW = {
    "windows-x86_64": 10 * MIB,
    "windows-aarch64": 8 * MIB,
    "macos-x86_64": 6 * MIB,
    "macos-aarch64": 4 * MIB,
    "linux-x86_64": 3 * MIB,
    "linux-aarch64": 1 * MIB,
}


class SizeCellsTest(unittest.TestCase):
    def test_normal_deltas(self):
        cells = rs.size_cells(FULL_OLD, FULL_NEW)
        self.assertEqual(cells["rust"], "")
        self.assertEqual(cells["apps/windows"], rs.fmt_size_bump(1 * MIB))  # +1.00 MiB
        self.assertEqual(cells["apps/macos"], rs.fmt_size_bump(-512 * KIB))  # -512 KiB
        self.assertEqual(cells["apps/linux"], rs.fmt_size_bump(0))  # +0 KiB
        # Total: six-package sums 31 MiB → 32 MiB, so means differ by 1/6 MiB.
        self.assertEqual(cells["Total"], rs.fmt_size_bump(MIB / 6))

    def test_no_new_sizes_gives_placeholders(self):
        for new_by in (None, {}):
            cells = rs.size_cells(FULL_OLD, new_by)
            self.assertEqual(cells["rust"], "")
            for label in ("apps/macos", "apps/windows", "apps/linux", "Total"):
                self.assertEqual(cells[label], "…")

    def test_no_old_sizes_gives_dashes(self):
        cells = rs.size_cells(None, FULL_NEW)
        self.assertEqual(cells["rust"], "")
        for label in ("apps/macos", "apps/windows", "apps/linux", "Total"):
            self.assertEqual(cells[label], "—")

    def test_partial_platforms(self):
        new_no_linux = {k: v for k, v in FULL_NEW.items() if not k.startswith("linux")}
        cells = rs.size_cells(FULL_OLD, new_no_linux)
        self.assertEqual(cells["apps/linux"], "—")
        self.assertEqual(cells["apps/windows"], rs.fmt_size_bump(1 * MIB))
        # Total averages the four common (non-linux) platforms: +0.25 MiB.
        self.assertEqual(cells["Total"], rs.fmt_size_bump(0.25 * MIB))


BODY_LF = (
    "# DontSpeak v0.3.3\n"
    "\n"
    "Some prose | with a pipe.\n"
    "\n"
    "## Lines\n"
    "\n"
    "https://github.com/delllusional/DontSpeak/compare/v0.3.2...v0.3.3\n"
    "\n"
    "| Area | Code | Tests | Comments | Binaries avg |\n"
    "|---|---:|---:|---:|---:|\n"
    "| `rust` | +10 / -2 | +5 / -1 | +3 / -0 |  |\n"
    "| `apps/macos` | +1 / -0 | +0 / -0 | +0 / -0 | … |\n"
    "| `apps/windows` | +2 / -1 | +0 / -0 | +1 / -0 | … |\n"
    "| `apps/linux` | +0 / -0 | +0 / -0 | +0 / -0 | … |\n"
    "| **Total** | **+13 / -3** | **+5 / -1** | **+4 / -0** | **…** |\n"
    "\n"
    "Trailing prose.\n"
)
CELLS = {
    "rust": "",
    "apps/macos": "-512 KiB",
    "apps/windows": "+1.00 MiB",
    "apps/linux": "—",
    "Total": "+0.25 MiB",
}


class PatchLinesTableTest(unittest.TestCase):
    def test_only_fifth_column_changes(self):
        patched = rs.patch_lines_table(BODY_LF, CELLS)
        self.assertIsNotNone(patched)
        old_lines = BODY_LF.splitlines(keepends=True)
        new_lines = patched.splitlines(keepends=True)
        self.assertEqual(len(old_lines), len(new_lines))
        for old, new in zip(old_lines, new_lines):
            if old == new:
                continue
            # Changed lines are table rows differing only in the fifth cell.
            self.assertEqual(old.split("|")[:5], new.split("|")[:5])
        self.assertIn("| `apps/macos` | +1 / -0 | +0 / -0 | +0 / -0 | -512 KiB |\n", patched)
        self.assertIn("| `apps/windows` | +2 / -1 | +0 / -0 | +1 / -0 | +1.00 MiB |\n", patched)
        self.assertIn("| `apps/linux` | +0 / -0 | +0 / -0 | +0 / -0 | — |\n", patched)
        self.assertIn(
            "| **Total** | **+13 / -3** | **+5 / -1** | **+4 / -0** | **+0.25 MiB** |\n",
            patched,
        )
        # rust stays blank; prose (even with a pipe) untouched.
        self.assertIn("| `rust` | +10 / -2 | +5 / -1 | +3 / -0 |  |\n", patched)
        self.assertIn("Some prose | with a pipe.\n", patched)

    def test_crlf_preserved(self):
        body = BODY_LF.replace("\n", "\r\n")
        patched = rs.patch_lines_table(body, CELLS)
        self.assertIsNotNone(patched)
        self.assertNotIn("…", patched)
        # Every newline still CRLF.
        self.assertEqual(patched.count("\n"), patched.count("\r\n"))
        self.assertEqual(patched.replace("\r\n", "\n"), rs.patch_lines_table(BODY_LF, CELLS))

    def test_no_lines_table_returns_none(self):
        self.assertIsNone(rs.patch_lines_table("chore: dev draft commit message\n", CELLS))

    def test_stale_values_overwritten(self):
        stale = BODY_LF.replace("| … |", "| +9.99 MiB |").replace("**…**", "**wrong**")
        patched = rs.patch_lines_table(stale, CELLS)
        self.assertNotIn("+9.99 MiB", patched)
        self.assertNotIn("wrong", patched)
        self.assertIn("**+0.25 MiB**", patched)

    def test_idempotent(self):
        once = rs.patch_lines_table(BODY_LF, CELLS)
        self.assertEqual(rs.patch_lines_table(once, CELLS), once)


class SpotChecksTest(unittest.TestCase):
    def test_platform_key(self):
        self.assertEqual(
            rs.platform_key("dontspeak-0.3.2-linux-x86_64.tar.gz"), "linux-x86_64"
        )
        self.assertEqual(
            rs.platform_key("dontspeak-0.3.2-macos-aarch64.app.zip"), "macos-aarch64"
        )
        self.assertIsNone(rs.platform_key("checksums.txt"))

    def test_fmt_size_bump(self):
        self.assertEqual(rs.fmt_size_bump(512 * KIB), "+512 KiB")
        self.assertEqual(rs.fmt_size_bump(-512 * KIB), "-512 KiB")
        self.assertEqual(rs.fmt_size_bump(MIB - 1), "+1024 KiB")  # just under threshold
        self.assertEqual(rs.fmt_size_bump(1.5 * MIB), "+1.50 MiB")
        self.assertEqual(rs.fmt_size_bump(-3 * MIB), "-3.00 MiB")
        self.assertEqual(rs.fmt_size_bump(0), "+0 KiB")


if __name__ == "__main__":
    unittest.main()
