//! Brands `ds-helper.exe` Windows version resource (Task Manager "DontSpeak") —
//! counterpart to WinUI `<AssemblyTitle>/<Product>`. Windows host+target only
//! (`cfg(windows)` build-dep + target gate); no-op elsewhere. Missing rc.exe → warning only.

fn main() {
    // Script reads no other files (CARGO_CFG_*/CARGO_PKG_* are fingerprint inputs).
    println!("cargo:rerun-if-changed=build.rs");
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
