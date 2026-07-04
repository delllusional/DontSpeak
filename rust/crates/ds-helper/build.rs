//! Brands the `ds-helper.exe` Windows version resource so Task Manager / Explorer "Details"
//! show "DontSpeak" instead of the bare "ds-helper" exe name — the Rust counterpart to the
//! WinUI app's `<AssemblyTitle>/<Product>` (apps/windows/winui/DontSpeak.WinUI.csproj).
//!
//! Windows-only and best-effort: the `winresource` build-dep is pulled only on a Windows HOST
//! (see Cargo.toml's `cfg(windows)` build-dependencies), and we additionally gate on a Windows
//! TARGET, so this build script is an empty no-op for every macOS/Linux build and never needs
//! rc.exe there. A missing rc.exe only drops the metadata (a warning), never fails the build.

fn main() {
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set("FileDescription", "DontSpeak");
            res.set("ProductName", "DontSpeak");
            res.set("OriginalFilename", "ds-helper.exe");
            if let Err(e) = res.compile() {
                println!("cargo:warning=ds-helper version resource not embedded: {e}");
            }
        }
    }
}
