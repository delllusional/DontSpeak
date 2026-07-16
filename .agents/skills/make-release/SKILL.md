---
name: make-release
description: Cut a DontSpeak release — tag the single-source version, push the tag to trigger release.yml on GitHub, MONITOR the ~25-30-min run (it can and does fail), verify the published assets, then deploy the site. Also covers re-cutting a failed release, and cutting/replacing an on-demand DRAFT release of the current `-dev` version for real installable dev binaries without officially shipping. Use when asked to release, cut/publish a version, re-release, cut a dev/preview/draft build, or when a release build failed.
---

# DontSpeak — make a release

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

Tag-triggered CI: push `v<version>` → `release.yml` gates, builds, publishes. Monitor
to completion and verify assets.

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
  (cd rust && cargo fmt) && (cd apps/linux/gtk && cargo fmt)
  (cd rust && cargo fmt --check) && (cd apps/linux/gtk && cargo fmt --check)
  (cd rust && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked)
  cargo deny --manifest-path rust/Cargo.toml --all-features check --config rust/deny.toml
  cargo deny --manifest-path apps/linux/gtk/Cargo.toml --all-features check --config rust/deny.toml
  ```
  Both workspaces, all-features deny. Commit fmt/advisory fixes.
- **macOS Swift tests** (not in prepush):
  ```bash
  (cd rust && cargo build --profile release-ffi --locked -p ds-core)
  (cd apps/macos && MACOSX_DEPLOYMENT_TARGET=14.0 swift test)
  ```
  Release also runs WinUI xunit on Windows.
- Push account with write to `delllusional/DontSpeak`.

## 2 — Tag and trigger

```bash
ver="$(python3 scripts/release/sync-workspace-version.py --print)"
git tag "v$ver"
git push origin main "v$ver"
```

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
4. **publish** — assets + fixed-name installers + checksums. `--generate-notes` is empty
   here (no PRs) — write notes yourself (step 6).

## 4 — Monitor (mandatory)

```bash
run_id=$(gh run list --repo delllusional/DontSpeak --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$run_id" --repo delllusional/DontSpeak --exit-status
```

On fail: `gh run view "$run_id" --log-failed`. Common: tag≠version, OS-only test
(e.g. Windows CRLF), toolchain drift, notarization secret. Fail before publish → tag
without release.

Watcher can drop mid-run (`wsarecv` / intermittent `HTTP 503`); restart watch or poll
`gh run view "$run_id" --json status,conclusion` every 30–60s — CI itself is fine.

## 5 — Verify

```bash
gh release view "v$ver" --repo delllusional/DontSpeak --json assets --jq '[.assets[].name]'
```

Expect **9 assets**: `checksums.txt`, `install.sh`, `install.ps1`, linux
`{x86_64,aarch64}.tar.gz`, macos `{aarch64,x86_64}.app.zip`, windows
`{x86_64,aarch64}.zip`. Missing Linux = best-effort job fail (re-cut or ship without).
Missing installer = release failure.

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

## 6 — Release notes

Write summary prose by hand. Stats are scripted:

1. Prev tag: `gh release list --limit 2`. Changes: `git log <prev>..v$ver --oneline`.
2. Sections that actually apply (Bug fixes / Features / Shared / per-OS). One plain line
   per change + commit link:
   `- <desc>. [`<sha>`](https://github.com/delllusional/DontSpeak/commit/<sha>)`
3. `## Lines` section: bare compare URL then
   `scripts/release/release-stats.py <prev> v$ver` table.
4. `gh release edit "v$ver" --repo delllusional/DontSpeak --notes-file <file>`

## 7 — Re-cut (pre-1.0 or explicit)

```bash
gh release delete "v$ver" --repo delllusional/DontSpeak --yes   # if published
git push origin ":refs/tags/v$ver" && git tag -d "v$ver"
git tag "v$ver" && git push origin main "v$ver"
```

Monitor again. Move tag only to the **release** commit — not later main work still on
the same version string.

## 8 — Deploy site

Mandatory: `deploy-site` skill in **dontspeak.org** checkout. Installers resolve latest
via API, so brief lag is OK — still don't skip.

## 9 — Bump next `-dev`

After publish, one command updates Cargo.toml + both locks (no full re-resolve):

```bash
python3 scripts/release/sync-workspace-version.py --bump-dev
# or: python3 scripts/release/sync-workspace-version.py --set 0.4.0-dev
(cd rust && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
(cd apps/linux/gtk && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
```

Commit all four files. Next real release: `--strip-dev` (or `--set` higher non-dev),
commit, tag. Missed sync fails only at **next** tag.

## 10 — On-demand `-dev` draft

Same full matrix, `--draft` so `releases/latest` ignores it. Replace prior draft tag
in place:

```bash
ver="$(python3 scripts/release/sync-workspace-version.py --print)"
case "$ver" in
  *-dev*) ;;
  *) echo "not a -dev version — use step 2 for real release" >&2; exit 1 ;;
esac
git push origin ":refs/tags/v$ver" 2>/dev/null || true
git tag -d "v$ver" 2>/dev/null || true
git tag "v$ver"
git push origin main "v$ver"
```

Monitor + verify; confirm `isDraft == true`. Skip notes, site deploy, version bump.
Web UI may show `untagged-<hash>` briefly — display-only; `tag_name` is correct.

## Caveats

- Failed tests after tag → tag without release; clean with step 7 or next push rejects.
- `workflow_dispatch` builds without publish.
- Don't `git tag -f` without deleting remote first.
- Don't skip step 9.
