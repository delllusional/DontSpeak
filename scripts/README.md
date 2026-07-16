# Scripts

Repository scripts are grouped by purpose so public entry points, package payloads,
developer workflows, and maintenance tools do not share one flat directory.

| Directory | Purpose |
| --- | --- |
| `install/web/` | Public, download-only installers used by the website and README one-liners. |
| `install/local/` | Source-checkout installers for local development builds. |
| `install/bundle/` | Canonical uninstallers copied into release archives and app bundles. |
| `install/lib/` | Shared shell functions sourced by installers and platform bundlers. |
| `release/` | Version, release-statistics, and version-sync tooling (`sync-workspace-version.py` is the portable four-file bump used by `make-release`). |
| `ci/` | CI report-processing helpers. |
| `agents/` | Commit-attribution and cross-agent skill-maintenance tooling. |

Run scripts from the repository root unless their own usage text says otherwise.
