//! Bakes DONTSPEAK_BUILD_ID for engine debug log (install/dist set git; else "dev").

fn main() {
    println!("cargo:rerun-if-env-changed=DONTSPEAK_BUILD_ID");
    let id = std::env::var("DONTSPEAK_BUILD_ID").unwrap_or_else(|_| "dev".to_string());
    println!("cargo:rustc-env=DONTSPEAK_BUILD_ID={id}");
}
