# Scripts

Repository scripts are grouped by purpose so public entry points, package payloads,
developer workflows, and maintenance tools do not share one flat directory.

| Directory | Purpose |
| --- | --- |
| `install/web/` | Public, download-only installers used by the website and README one-liners. |
| `install/local/` | Source-checkout installers for local development builds. |
| `install/bundle/` | Canonical uninstallers copied into release archives and app bundles. |
| `install/lib/` | Shared shell functions sourced by installers and platform bundlers. |
| `release/` | Version tooling: `sync-workspace-version.py` (print / strip-dev / bump-dev / set + four-file lock sync) and `release-stats.py` (release-notes Lines table; `--patch-sizes` fills the Binaries $\overline{\Delta}$ cells post-publish). |
| `ci/` | CI report-processing helpers and the ASCII gate (`check-shell-ascii.mjs`). |
| `agents/` | Commit-attribution and cross-agent skill-maintenance tooling. |

Run scripts from the repository root unless their own usage text says otherwise.

Every `.sh` and `.ps1` in the repository is ASCII-only: none carries a BOM, so a
console decodes them by its own codepage and anything else reaches the user as
mojibake. `node scripts/ci/check-shell-ascii.mjs` enforces it (release gate in
`release.yml`); run it locally after editing any script.
