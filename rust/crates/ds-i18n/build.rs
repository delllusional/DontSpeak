// Force recompile when the embedded locale catalog changes.
//
// `rust_i18n::i18n!("locales", …)` bakes `locales/*.yml` at COMPILE time. Cargo only
// re-expands that macro when this crate's Rust sources change, so a `.yml`-only edit
// would leave a STALE catalog (missing keys render as raw key strings). Tracking
// `locales` makes cargo rebuild whenever a translation file changes.
fn main() {
    println!("cargo:rerun-if-changed=locales");
}
