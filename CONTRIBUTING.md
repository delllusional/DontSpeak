# Contributing to DontSpeak

Rust engine/CLI in `rust/` (25 crates); thin hosts: SwiftUI (`apps/macos/`), WinUI 3
(`apps/windows/winui/`), GTK4 (`apps/linux/gtk/`). See [ARCHITECTURE.md](ARCHITECTURE.md)
and [docs/BUILD-DEPLOY.md](docs/BUILD-DEPLOY.md) before testing a running app — wrong
rebuild leaves stale code.

## Build prerequisites

**Everywhere:** [rustup](https://rustup.rs) (`rust-version = 1.97`). Build with
`--locked` — CI rejects lock drift.

**macOS** — full Xcode (not CLT only); first-launch setup:

```sh
xcode-select -s /Applications/Xcode.app
sudo xcodebuild -runFirstLaunch
```

`./apps/macos/build.sh` (dev) or `./apps/macos/bundle.sh` (app bundle). Module-cache
error after clone: `rm -rf apps/macos/.build` and retry.

**Linux** — ALSA + Pulse for the workspace:

```sh
sudo apt-get install -y build-essential pkg-config libasound2-dev libpulse-dev
```

GTK host needs GTK 4.12+, **libadwaita ≥ 1.7**, gtk4-layer-shell (Ubuntu 26.04 /
Fedora 42 era; 24.04 too old):

```sh
sudo apt-get install -y libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev
```

**Windows** — NASM (x64) or clang (arm64) for `ring`; .NET 10 SDK:

- `choco install nasm`, LLVM on `PATH`
- `dotnet build apps/windows/winui/DontSpeak.WinUI.csproj -c Release -p:Platform=x64`

## Tests

```sh
cd rust && cargo test --workspace --locked
```

macOS: `cd apps/macos && swift test` (needs FFI staticlib; `build.sh` builds it).
WinUI: `apps/windows/winui.tests`. CI: clippy + Rust tests per commit; wider matrix
on release tags (`.github/workflows/ci.yml`).

## Gates

```sh
cd rust
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

`prepush` skill = per-commit procedure. fmt, rustdoc, deny, platform hosts = release
gates (`make-release`). Shell: `bash -n` + `shellcheck`; workflows: `actionlint`
(`.github/actionlint.yaml`). C#: `csharp.yml` path-filtered, keep warning-clean.

Lint policy: Rust in `[workspace.lints]` (`rust/Cargo.toml`); don't add bare
`#[allow]` without why. Swift: `apps/macos/.swift-format`.
