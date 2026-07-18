---
name: make-release
description: Cut a DontSpeak release — tag the single-source version with annotated release notes, push the tag to trigger release.yml on GitHub, MONITOR the ~25-30-min run (it can and does fail), verify the published assets, then bump the next `-dev` version. Also covers re-cutting a failed release, and cutting/replacing an on-demand DRAFT release of the current `-dev` version for real installable dev binaries without officially shipping. Use when asked to release, cut/publish a version, re-release, cut a dev/preview/draft build, or when a release build failed.
---

# DontSpeak — make a release

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

Tag-triggered CI: push annotated `v<version>` → `release.yml` gates, builds, publishes
with the **tag annotation as the release body** (`gh release create --notes-from-tag`).
Monitor to completion and verify assets. Notes are **not** committed to the tree and
**not** pasted after publish.

## 1 — Preconditions

- **Version** = `rust/Cargo.toml` `[workspace.package] version`, managed by
  `scripts/release/sync-workspace-version.py`. Tag must be `v` + that exact string.
  Strip `-dev` with the script (not hand-edit of four files, **not**
  `cargo generate-lockfile` — that re-resolves registry deps and causes huge lock churn):
  ```bash
  python3 scripts/release/sync-workspace-version.py --strip-dev
  (cd rust && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
  (cd apps/linux/gtk && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
  ```
  Escalate patch→minor/major only deliberately. Diff locks: only workspace package
  `version =` lines (~one per crate). No lingering `-dev` for a real release.
- **Green main**: `prepush` first; tagged commit on `origin/main`. Local prepush is
  fail-fast; release full-matrix tests/hygiene are the real gate.
- **Hygiene** (top re-cut cause — not in per-commit CI):
  ```bash
  (cd rust && cargo fmt --all) && (cd apps/linux/gtk && cargo fmt --all)
  (cd rust && cargo fmt --all --check) && (cd apps/linux/gtk && cargo fmt --all --check)
  (cd rust && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked)
  cargo deny --manifest-path rust/Cargo.toml --all-features check --config rust/deny.toml
  cargo deny --manifest-path apps/linux/gtk/Cargo.toml --all-features check --config rust/deny.toml
  ```
  Both workspaces, all-features deny. Commit fmt/advisory fixes. Use `cargo fmt --all`
  from each package root (not bare `--manifest-path` from repo root — that misses targets).

- **Dependency / deny policy** (when hygiene deny fails, or after any lock update):
  1. **Prefer current crate versions via Cargo only.** Do not pin/downgrade to silence
     deny (e.g. hold `rust-i18n` at 4.1). Bump **both** locks together with scoped updates:
     ```bash
     (cd rust && cargo update -p rust-i18n -p rust-i18n-macro -p rust-i18n-support)
     (cd apps/linux/gtk && cargo update -p rust-i18n -p rust-i18n-macro -p rust-i18n-support)
     (cd rust && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
     (cd apps/linux/gtk && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
     cargo tree -i <crate> --manifest-path rust/Cargo.toml --locked
     cargo tree -i <crate> --manifest-path apps/linux/gtk/Cargo.toml --locked
     ```
     No hand-edited lock edges and no custom rewiring scripts.
  2. **Cargo multi-version edges are intentional.** Resolver maximizes versions *and*
     minimizes how many copies of a crate stay in the graph. If `ring` needs
     `windows-sys ^0.52` and `tempfile` accepts `>=0.52, <0.62`, Cargo may point
     tempfile at **0.52** to share ring's line — that is not a bug and not a
     "downgrade" to fight. Real caps stay (e.g. `jni` → 0.45, `notify` → ^0.60).
     Inspect with `cargo tree -i windows-sys@0.52.0` / `@0.61.2`.
  3. **Diagnose before editing `rust/deny.toml`.**
     `cargo tree -i <dup>@<ver> --locked` on **both** manifests. If one lock is simply
     behind, `cargo update -p …` on that workspace — real fix, not a skip.
  4. **`[bans].skip` only for irreducible multi-version splits** after preferring new
     deps (semver-incompatible parents: ahash→getrandom 0.3 vs rand 0.10→0.4). Reason
     must name the actual parents. Never skip for "GTK failed / rust passed" without
     checking lock drift first.
  5. **Cleanup pass on every release.** Re-run deny; drop `skip` rows that show
     `unmatched-skip` **and** still pass both denies without the row (keep only if the
     other workspace's `--all-features` graph still needs it — e.g. cbindgen TOML).
     Prefer deleting stale skips over accumulating them.
- **macOS Swift tests** (not in prepush):
  ```bash
  (cd rust && cargo build --profile release-ffi --locked -p ds-core)
  (cd apps/macos && MACOSX_DEPLOYMENT_TARGET=14.0 swift test)
  ```
  Release also runs WinUI xunit on Windows.
- **Release notes ready** (real releases only — before tagging). Write the body that
  will become the GitHub Release description into a **local temp file** (never commit
  it). See step 2 for format.
- Push account with write to `delllusional/DontSpeak`.

## 2 — Notes, annotated tag, and trigger

1. Find previous release: `gh release list --limit 2`. Changes:
   `git log <prev>..HEAD --oneline` (after the version commit is on main).
2. Write notes to a temp file (e.g. `/tmp/dontspeak-v$ver-notes.md` or
   `$env:TEMP\dontspeak-notes.md` on Windows). Sections that actually apply
   (Bug fixes / Features / Shared / per-OS). One plain line per change + commit link:
   `- <desc>. [`<sha>`](https://github.com/delllusional/DontSpeak/commit/<sha>)`
3. Append `## Lines`: bare compare URL then the table from
   `scripts/release/release-stats.py <prev> v$ver` (use the version string that will be
   tagged; after the version commit lands, `v$ver` matches HEAD for stats against prev).
   Table columns: Area / Code / Tests / Comments / **Binaries avg** — host app rows
   show mean package size delta for that OS’s two arches; **Total** is the six-package
   mean; `rust` is blank. Sizes from `gh release view` (install scripts / checksums
   excluded). Needs both tags published (drafts OK).
4. **Annotated tag** (required for real releases — body is the release notes; not a
   tree file). Then push:

```bash
ver="$(python3 scripts/release/sync-workspace-version.py --print)"
# notes_file = path written above (local only; not in git)
# Disable comment stripping so markdown `##` section headers survive (git default
# treats lines starting with `#` as comments and drops them from the annotation).
git -c core.commentChar=';' tag -a "v$ver" -F "$notes_file"
# Windows PowerShell fallback if null is rejected: git -c core.commentChar=";" tag -a ...
git push origin main "v$ver"
```

Lightweight `git tag "v$ver"` is wrong for a real release: CI would fall back to the
version-bump commit message as the body.

Drop superseding `-dev` drafts (query, don't guess tags):

```bash
gh api --paginate "repos/delllusional/DontSpeak/releases" --jq '.[] | select(.draft==true and (.tag_name | test("-dev"))) | "\(.id) \(.tag_name)"' |
while read -r id tag; do
  echo "Removing stale dev draft: $tag (id=$id)"
  gh api -X DELETE "repos/delllusional/DontSpeak/releases/$id"
  git push origin ":refs/tags/$tag" 2>/dev/null || true
  git tag -d "$tag" 2>/dev/null || true
done
```

## 3 — What runs (`release.yml`)

1. **check** — tag == Cargo version
2. **tests** — full OS matrix + hygiene (fmt both + rustdoc)
3. **builds** (parallel): Windows portable zips (unsigned); macOS signed/notarized if
   `APPLE_*` secrets else ad-hoc; Linux tarballs (upload best-effort)
4. **publish** — assets + fixed-name installers + checksums + body from
   `--notes-from-tag` (the annotation written in step 2)

## 4 — Monitor (mandatory)

```bash
run_id=$(gh run list --repo delllusional/DontSpeak --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$run_id" --repo delllusional/DontSpeak --exit-status
```

On fail: `gh run view "$run_id" --log-failed`. Common: tag≠version, OS-only test
(e.g. Windows CRLF), toolchain drift, notarization secret, or (fixed) publish using
`--notes-from-tag` together with `--repo` — gh rejects that pair; publish must rely
on the job's default repo context. Fail before publish → tag without release; if
builds succeeded, download artifacts and `gh release create` with `--notes-file`
from the tag body, then still fix the workflow on main.

Watcher can drop mid-run (`wsarecv` / intermittent `HTTP 503`); restart watch or poll
`gh run view "$run_id" --json status,conclusion` every 30–60s — CI itself is fine.

## 5 — Verify

```bash
gh release view "v$ver" --repo delllusional/DontSpeak --json assets,body --jq '{assets:[.assets[].name], body_preview:(.body[:200])}'
```

Expect **9 assets**: `checksums.txt`, `install.sh`, `install.ps1`, linux
`{x86_64,aarch64}.tar.gz`, macos `{aarch64,x86_64}.app.zip`, windows
`{x86_64,aarch64}.zip`. Missing Linux = best-effort job fail (re-cut or ship without).
Missing installer = release failure. Body should match the annotation (not empty
auto-notes).

```bash
tmp="$(mktemp -d)"
gh release download "v$ver" --repo delllusional/DontSpeak \
  --pattern install.sh --pattern install.ps1 --dir "$tmp"
cmp "$tmp/install.sh" scripts/install/web/install.sh
cmp "$tmp/install.ps1" scripts/install/web/install.ps1
rm -rf "$tmp"
```

Published only — latest endpoints:

```bash
curl -fsSLI https://github.com/delllusional/DontSpeak/releases/latest/download/install.sh
curl -fsSLI https://github.com/delllusional/DontSpeak/releases/latest/download/install.ps1
```

Skip latest check for drafts (they don't move `releases/latest`).

## 6 — Re-cut (pre-1.0 or explicit)

```bash
gh release delete "v$ver" --repo delllusional/DontSpeak --yes   # if published
git push origin ":refs/tags/v$ver" && git tag -d "v$ver"
# Re-apply the same notes file (or a fixed one), still annotated:
git -c core.commentChar=';' tag -a "v$ver" -F "$notes_file"
git push origin main "v$ver"
```

Monitor again. Move tag only to the **release** commit — not later main work still on
the same version string.

## 7 — Bump next `-dev`

After publish, one command updates Cargo.toml + both locks (no full re-resolve):

```bash
python3 scripts/release/sync-workspace-version.py --bump-dev
# or: python3 scripts/release/sync-workspace-version.py --set 0.4.0-dev
(cd rust && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
(cd apps/linux/gtk && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
```

Commit all four files. Next real release: `--strip-dev` (or `--set` higher non-dev),
commit, annotated-tag, push. Missed sync fails only at **next** tag.

## 8 — On-demand `-dev` draft

Same full matrix, `--draft` so `releases/latest` ignores it. Replace prior draft tag
in place. Notes optional — lightweight is OK (body falls back to commit message):

```bash
ver="$(python3 scripts/release/sync-workspace-version.py --print)"
case "$ver" in
  *-dev*) ;;
  *) echo "not a -dev version — use step 2 for real release" >&2; exit 1 ;;
esac
git push origin ":refs/tags/v$ver" 2>/dev/null || true
git tag -d "v$ver" 2>/dev/null || true
git tag "v$ver"   # lightweight OK for disposable drafts
git push origin main "v$ver"
```

Monitor + verify; confirm `isDraft == true`. Skip version bump.
Web UI may show `untagged-<hash>` briefly — display-only; `tag_name` is correct.

## Caveats

- Failed tests after tag → tag without release; clean with step 6 or next push rejects.
- `workflow_dispatch` builds without publish.
- Don't `git tag -f` without deleting the remote tag first.
- Don't skip step 7 after a real publish.
- Real releases: always `git tag -a -F …`. Never commit release notes into the tree
  for CI to read — the annotation is the transport.
