# Contributing to DontSpeak

The engine and CLI are Rust (`rust/`, 15 crates, one workspace); each OS gets a thin native
host — SwiftUI (`apps/macos/`), WinUI 3 (`apps/windows/winui/`), GTK4 (`apps/linux/gtk/`).
Read [ARCHITECTURE.md](ARCHITECTURE.md) for how the pieces fit, and
[docs/BUILD-DEPLOY.md](docs/BUILD-DEPLOY.md) before testing a change against the *running*
app — the three runtime pieces deploy by different routes, and using the wrong one leaves
the app running stale code.

## Build prerequisites

**Everywhere:** a Rust toolchain via [rustup](https://rustup.rs) (workspace pins
`rust-version = 1.96`; plain `rustup default stable` is fine). Build with `--locked` —
`Cargo.lock` is committed and CI rejects drift.

**macOS** — the SwiftUI host needs the **full Xcode**, not just the Command Line Tools
(asset-catalog compilation crashes under bare CLT), and Xcode must have completed its
first-launch setup:

```sh
xcode-select -s /Applications/Xcode.app
sudo xcodebuild -runFirstLaunch
```

Then `./apps/macos/build.sh` (dev build) or `./apps/macos/bundle.sh` (full app bundle).
If the very first `swift build` fails with a module-cache error after moving/cloning the
repo, `rm -rf apps/macos/.build` and retry.

**Linux** — the Rust workspace links ALSA (`cpal`) and PulseAudio (`ds-aec`):

```sh
sudo apt-get install -y build-essential pkg-config libasound2-dev libpulse-dev
```

The GTK host additionally needs a recent GNOME stack — GTK 4.12+, **libadwaita >= 1.7**,
gtk4-layer-shell (Ubuntu 26.04 / Fedora 42 era; Ubuntu 24.04 is too old):

```sh
sudo apt-get install -y libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev
```

**Windows** — `ring` (rustls' crypto) assembles via NASM on x64 and clang on arm64; the
WinUI app targets .NET 10:

- NASM (`choco install nasm`) and LLVM on `PATH`
- .NET 10 SDK
- `dotnet build apps/windows/winui/DontSpeak.WinUI.csproj -c Release -p:Platform=x64`

## Tests

The real test suite lives in the Rust workspace:

```sh
cd rust && cargo test --workspace --locked
```

The macOS host has SwiftPM logic tests (`cd apps/macos && swift test` — needs the Rust
FFI staticlib built first; `build.sh` does that). The WinUI app has no test projects yet.
CI runs Linux per commit and the full ubuntu + windows + macOS matrix on release tags
(`.github/workflows/ci.yml`).

## Gates (run these before pushing)

CI rejects anything that fails:

```sh
cd rust
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cd ../apps/macos
swift format lint --strict --recursive Sources SmKokoro/Sources Tests   # config: apps/macos/.swift-format
```

Shell scripts should stay clean under `bash -n` and `shellcheck`; workflows under
`actionlint` (config: `.github/actionlint.yaml`). C# analyzers run in the path-filtered
`csharp.yml` job — keep the build warning-clean.

Lint policy is centralized: Rust lints live in `[workspace.lints]` in `rust/Cargo.toml`
(don't add per-crate `#[allow]`s without a comment saying why), Swift formatting in
`apps/macos/.swift-format`.
