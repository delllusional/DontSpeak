#!/usr/bin/env bash
# sync-gtk-version.sh — propagate rust/Cargo.toml's [workspace.package] version into
# apps/linux/gtk/Cargo.toml's standalone `version = "..."`, which can't use
# `version.workspace = true` since that's a separate Cargo workspace (see its own
# header comment). Called by make-release step 9 instead of a hand edit.
#
# Portability: no GNU-only `sed -i` (BSD/macOS sed requires a backup-suffix arg) and
# no GNU-only `0,/re/` first-match address (BSD/macOS sed errors on it) — the target
# line is unique in the file, so a plain anchored substitution needs neither.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
gtk_cargo="$here/../apps/linux/gtk/Cargo.toml"

target="$(bash "$here/version.sh")"
current="$(grep -m1 '^version = "' "$gtk_cargo" 2>/dev/null | sed -E 's/version = "([^"]+)"/\1/')" || current=""
if [ -z "$current" ]; then
    echo "sync-gtk-version: no 'version = \"...\"' line found in $gtk_cargo" >&2
    exit 1
fi

if [ "$current" = "$target" ]; then
    echo "apps/linux/gtk/Cargo.toml already in sync at $target"
    exit 0
fi

tmp="$(mktemp)"
sed -E "s/^version = \"[^\"]*\"/version = \"$target\"/" "$gtk_cargo" > "$tmp" && mv "$tmp" "$gtk_cargo"

after="$(grep -m1 '^version = "' "$gtk_cargo" | sed -E 's/version = "([^"]+)"/\1/')"
if [ "$after" != "$target" ]; then
    echo "sync-gtk-version: substitution failed — expected version = \"$target\" in $gtk_cargo, got \"$after\"" >&2
    exit 1
fi

echo "apps/linux/gtk/Cargo.toml: $current -> $target"
