#!/usr/bin/env python3
"""sync-workspace-version.py — bump/sync the marketing version across both Cargo workspaces.

Single source of truth: rust/Cargo.toml → [workspace.package] version.
Propagates into:
  • apps/linux/gtk/Cargo.toml (standalone workspace; cannot use version.workspace)
  • rust/Cargo.lock and apps/linux/gtk/Cargo.lock (workspace path-package version lines only)

Why this exists (and why make-release must NOT use `cargo generate-lockfile` for a
version bump): generate-lockfile re-resolves every registry dependency to the latest
compatible version, which is unrelated churn for a release. Path packages have no
registry checksum tied to their marketing version string — only their version field
in the lock needs to match Cargo.toml. This script updates those fields surgically.

After running, verify with:
  (cd rust && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
  (cd apps/linux/gtk && cargo metadata --format-version 1 --locked --no-deps >/dev/null)

Usage (from repo root):
  scripts/release/sync-workspace-version.py              # sync gtk + locks to rust version
  scripts/release/sync-workspace-version.py --strip-dev  # 0.3.1-dev → 0.3.1 then sync
  scripts/release/sync-workspace-version.py --bump-dev   # 0.3.1 → 0.3.2-dev then sync
  scripts/release/sync-workspace-version.py --set 0.4.0  # set exact version then sync
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RUST_CARGO = ROOT / "rust" / "Cargo.toml"
GTK_CARGO = ROOT / "apps" / "linux" / "gtk" / "Cargo.toml"
RUST_LOCK = ROOT / "rust" / "Cargo.lock"
GTK_LOCK = ROOT / "apps" / "linux" / "gtk" / "Cargo.lock"

VERSION_LINE_RE = re.compile(r'^version = "([^"]+)"', re.MULTILINE)
PACKAGE_BLOCK_RE = re.compile(
    r'(?ms)^\[\[package\]\]\n(.*?)(?=^\[\[|\Z)'
)
PKG_NAME_RE = re.compile(r'^name = "([^"]+)"', re.MULTILINE)
PKG_VERSION_RE = re.compile(r'^version = "([^"]+)"', re.MULTILINE)
PKG_SOURCE_RE = re.compile(r'(?m)^source = ')


def die(msg: str, code: int = 1) -> None:
    print(f"sync-workspace-version: {msg}", file=sys.stderr)
    raise SystemExit(code)


def read_text(path: Path) -> str:
    if not path.is_file():
        die(f"missing {path.relative_to(ROOT)}")
    return path.read_text(encoding="utf-8")


def write_text(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8", newline="\n")


def workspace_package_version(toml: str) -> str:
    # Prefer the [workspace.package] table's version over any other version = line.
    m = re.search(
        r'(?ms)^\[workspace\.package\]\s*\n(.*?)(?=^\[|\Z)',
        toml,
    )
    block = m.group(1) if m else toml
    vm = VERSION_LINE_RE.search(block)
    if not vm:
        die("no version = \"...\" under [workspace.package] in rust/Cargo.toml")
    return vm.group(1)


def set_workspace_package_version(toml: str, version: str) -> str:
    m = re.search(
        r'(?ms)^(\[workspace\.package\]\s*\n)(.*?)(?=^\[|\Z)',
        toml,
    )
    if not m:
        die("no [workspace.package] table in rust/Cargo.toml")
    head, body = m.group(1), m.group(2)
    if not VERSION_LINE_RE.search(body):
        die("no version line in [workspace.package]")
    new_body = VERSION_LINE_RE.sub(f'version = "{version}"', body, count=1)
    return toml[: m.start()] + head + new_body + toml[m.end() :]


def set_package_version(toml: str, version: str) -> str:
    """Replace the first package-level version = line (GTK standalone crate)."""
    if not VERSION_LINE_RE.search(toml):
        die("no version = \"...\" line found")
    return VERSION_LINE_RE.sub(f'version = "{version}"', toml, count=1)


def package_version(toml: str) -> str:
    m = VERSION_LINE_RE.search(toml)
    if not m:
        die("no version = \"...\" line found")
    return m.group(1)


def strip_dev(version: str) -> str:
    if version.endswith("-dev"):
        return version[: -len("-dev")]
    die(f"version {version!r} has no -dev suffix to strip")


def bump_dev(version: str) -> str:
    """0.3.1 or 0.3.1-dev → 0.3.2-dev (patch +1, always -dev)."""
    base = version[: -len("-dev")] if version.endswith("-dev") else version
    parts = base.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        die(f"cannot bump non-semver version {version!r}")
    major, minor, patch = (int(p) for p in parts)
    return f"{major}.{minor}.{patch + 1}-dev"


def workspace_member_paths(toml: str) -> list[str]:
    """Parse `members = [ ... ]` line-wise so a `]` inside a comment cannot truncate it.

    rust/Cargo.toml's members list has comments that mention other tables
    (e.g. `[workspace.dependencies]`); a non-greedy `\\[.*?\\]` regex stops at the
    first such bracket and silently drops later crates.
    """
    paths: list[str] = []
    in_members = False
    for raw in toml.splitlines():
        if not in_members:
            if re.match(r"^members\s*=\s*\[", raw):
                in_members = True
                # Rare: members = ["a"] on one line.
                code = raw.split("#", 1)[0]
                after = code.split("[", 1)[1]
                paths.extend(re.findall(r'"([^"]+)"', after))
                if "]" in after:
                    break
            continue
        code = raw.split("#", 1)[0]
        if "]" in code:
            paths.extend(re.findall(r'"([^"]+)"', code.split("]", 1)[0]))
            break
        paths.extend(re.findall(r'"([^"]+)"', code))
    if not paths:
        die("no members = [...] entries in rust/Cargo.toml")
    return paths


def workspace_crate_names() -> set[str]:
    """Package names of path crates in the rust workspace + the GTK host crate."""
    toml = read_text(RUST_CARGO)
    names: set[str] = set()
    for raw in workspace_member_paths(toml):
        # members are paths like "crates/ds-config"
        crate_toml = ROOT / "rust" / raw / "Cargo.toml"
        if not crate_toml.is_file():
            die(f"workspace member missing Cargo.toml: {raw}")
        cm = re.search(r'(?m)^name = "([^"]+)"', read_text(crate_toml))
        if not cm:
            die(f"no package name in {crate_toml.relative_to(ROOT)}")
        names.add(cm.group(1))
    # GTK host is a separate workspace that path-depends on the rust crates.
    gtk_name_m = re.search(r'(?m)^name = "([^"]+)"', read_text(GTK_CARGO))
    if gtk_name_m:
        names.add(gtk_name_m.group(1))
    if not names:
        die("resolved empty workspace package name set")
    return names


def is_path_package(block: str) -> bool:
    """Registry/git packages declare source =; workspace path packages do not."""
    return PKG_SOURCE_RE.search(block) is None


def rewrite_lock_versions(
    lock_text: str,
    names: set[str],
    old_version: str,
    new_version: str,
) -> tuple[str, int]:
    """Update version = for workspace path-package blocks.

    Matches by package name (from rust workspace members + GTK host) OR by a
    path package still carrying the previous marketing version — so a partial
    members parse cannot leave crates behind, and registry crates that happen
    to share a semver (e.g. dispatch2 0.3.1) are never touched.
    """
    changed = 0

    def repl(match: re.Match[str]) -> str:
        nonlocal changed
        block = match.group(0)
        nm = PKG_NAME_RE.search(block)
        vm = PKG_VERSION_RE.search(block)
        if not nm or not vm:
            return block
        name, cur = nm.group(1), vm.group(1)
        if cur == new_version:
            return block
        named = name in names
        path_old = is_path_package(block) and cur == old_version
        if not named and not path_old:
            return block
        # Named workspace crate must move even if its lock version already drifted.
        if named or path_old:
            changed += 1
            return (
                block[: vm.start()]
                + f'version = "{new_version}"'
                + block[vm.end() :]
            )
        return block

    new_text = PACKAGE_BLOCK_RE.sub(repl, lock_text)
    return new_text, changed


def apply_version(version: str) -> None:
    names = workspace_crate_names()

    rust_toml = read_text(RUST_CARGO)
    old_rust = workspace_package_version(rust_toml)
    if old_rust != version:
        write_text(RUST_CARGO, set_workspace_package_version(rust_toml, version))
        print(f"rust/Cargo.toml: {old_rust} -> {version}")
    else:
        print(f"rust/Cargo.toml already {version}")

    gtk_toml = read_text(GTK_CARGO)
    old_gtk = package_version(gtk_toml)
    if old_gtk != version:
        write_text(GTK_CARGO, set_package_version(gtk_toml, version))
        print(f"apps/linux/gtk/Cargo.toml: {old_gtk} -> {version}")
    else:
        print(f"apps/linux/gtk/Cargo.toml already {version}")

    # Locks may still show either the previous rust marketing version or a
    # drifted GTK-only string; rewrite accepts path packages at old_rust.
    for lock_path in (RUST_LOCK, GTK_LOCK):
        text = read_text(lock_path)
        new_text, n = rewrite_lock_versions(text, names, old_rust, version)
        rel = lock_path.relative_to(ROOT).as_posix()
        if n:
            write_text(lock_path, new_text)
            print(f"{rel}: updated {n} workspace package version(s) -> {version}")
        else:
            print(f"{rel}: workspace package versions already {version}")

    # Fail closed if any named workspace package still mismatches.
    leftover = []
    for lock_path in (RUST_LOCK, GTK_LOCK):
        text = read_text(lock_path)
        for block_m in PACKAGE_BLOCK_RE.finditer(text):
            block = block_m.group(0)
            nm = PKG_NAME_RE.search(block)
            vm = PKG_VERSION_RE.search(block)
            if nm and vm and nm.group(1) in names and vm.group(1) != version:
                leftover.append(f"{lock_path.name}:{nm.group(1)}={vm.group(1)}")
    if leftover:
        die("workspace packages still mismatched after rewrite: " + ", ".join(leftover))


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    g = p.add_mutually_exclusive_group()
    g.add_argument("--set", metavar="VERSION", help="set this exact version, then sync")
    g.add_argument("--strip-dev", action="store_true", help="strip -dev suffix, then sync")
    g.add_argument("--bump-dev", action="store_true", help="patch+1 and append -dev, then sync")
    args = p.parse_args(argv)

    current = workspace_package_version(read_text(RUST_CARGO))
    if args.set is not None:
        target = args.set
    elif args.strip_dev:
        target = strip_dev(current)
    elif args.bump_dev:
        target = bump_dev(current)
    else:
        target = current

    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-dev)?", target):
        die(f"refusing non-semver marketing version {target!r}")

    apply_version(target)
    print(f"ok: workspace marketing version is {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
