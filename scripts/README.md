# Scripts

Repository scripts are grouped by purpose so public entry points, package payloads,
developer workflows, and maintenance tools do not share one flat directory.

| Directory | Purpose |
| --- | --- |
| `install/web/` | Public, download-only installers used by the website and README one-liners. |
| `install/local/` | Source-checkout installers for local development builds. |
| `install/bundle/` | Canonical uninstallers copied into release archives and app bundles. |
| `install/lib/` | Shared shell functions sourced by installers and platform bundlers. |
| `release/` | Version tooling: `sync-workspace-version.py` (print / strip-dev / bump-dev / set + four-file lock sync) and `release-stats.py` (release-notes Lines table; `--patch-sizes` fills the Binaries-avg cells post-publish). |
| `ci/` | CI report-processing helpers. |
| `agents/` | Commit-attribution and cross-agent skill-maintenance tooling. |

Run scripts from the repository root unless their own usage text says otherwise.
