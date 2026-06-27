//  build.rs — force a recompile when the embedded locale catalog changes.
//
//  `rust_i18n::i18n!("locales", …)` bakes `locales/*.yml` into the binary at COMPILE
//  time via a proc macro. Cargo only re-expands that macro when ds-i18n's own Rust
//  sources change, so editing a `.yml` ALONE would otherwise leave the built rlib (and
//  the staticlib/cdylib that link it) carrying a STALE catalog — new keys then render
//  as their raw key string in the UI. Tracking the locales dir makes cargo rebuild this
//  crate whenever a translation file changes.
fn main() {
    println!("cargo:rerun-if-changed=locales");
}
