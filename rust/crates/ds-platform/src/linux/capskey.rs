//! "Own the Caps key" on Linux — neutralize its caps-lock TOGGLE at the keymap (XKB)
//! level so a physical press never enables capitals. The Linux counterpart of macOS
//! `capskey.rs` (hidutil null remap) and the Windows `WH_KEYBOARD_LL` suppression.
//!
//! Layering — why this keeps everything else in `linux.rs` working: XKB options live
//! ABOVE evdev. The kernel device keeps emitting `KEY_CAPSLOCK` (so `pump_caps_events`
//! still sees every down/up) while the compositor stops toggling the lock state — and
//! stops driving the Caps LED, which makes the LED purely OURS to drive as the
//! recording indicator (`set_caps_lock`'s EV_LED write), with no compositor fighting
//! it. Same *runtime* end state as the other two platforms — but NOT the same crash
//! residue (see the ownership-marker note below).
//!
//! ALL desktop detection and per-desktop plumbing lives in this file — one bucket, one
//! applier, nothing scattered:
//!   * GNOME (X11 or Wayland): `gsettings` read-merge-write of
//!     `org.gnome.desktop.input-sources xkb-options` (mutter applies it live).
//!   * KDE Plasma (X11 or Wayland): `kwriteconfig6/5` read-merge-write of
//!     `kxkbrc [Layout] Options` + the `org.kde.keyboard.reloadConfig` D-Bus signal.
//!   * Generic X11 (XFCE, MATE, Cinnamon, i3, …): `setxkbmap -option caps:none`
//!     (volatile, per-X-server; some DE settings daemons may reapply their own keymap —
//!     for those the GNOME/KDE buckets above catch the big ones).
//!   * Other Wayland compositors (sway, Hyprland, river, …): a client CANNOT change the
//!     compositor's keymap — DEGRADED, like the Wayland focus gate in `linux.rs`: log a
//!     one-line config instruction; Caps keeps toggling until the user adds the option.
//!   * Headless / no session: no-op.
//!
//! Ownership marker: GNOME/KDE settings are PERSISTENT (unlike macOS's per-login
//! hidutil map), so release must never strip a `caps:none` the USER had set themselves.
//! A marker file (`caps-owned` under `$XDG_STATE_HOME/dontspeak`, matching ds-config's
//! state root) records that WE added the option and to which bucket; `release_caps_key`
//! only removes the option when the marker exists, and a pre-existing `caps:none`
//! without a marker is treated as the user's and left alone (the key already doesn't
//! toggle — nothing to do). A hard SIGKILL leaves the option applied + the marker in
//! place, and the next clean run (or exit) reconciles. UNLIKE the other platforms this
//! residue is NOT self-healing: Windows unhooks `WH_KEYBOARD_LL` at process exit (zero
//! residue) and macOS's hidutil map is per-login (clears at next logout), but the
//! GNOME/KDE stores here are PERSISTENT across reboots — so "caps doesn't toggle"
//! survives a reboot until a clean run OR `uninstall.sh` step 4c strips it (generic X11
//! is volatile like macOS). The marker + next-run reconcile + uninstaller together are
//! what close that gap; `own`/`release` are written so the marker and the option can
//! never diverge into a permanent orphan (marker written before the option lands;
//! marker cleared only once the strip is confirmed).

use std::path::PathBuf;
use std::process::Command;

/// The XKB option that removes the caps-lock action from the Caps key while the evdev
/// device keeps reporting the physical press.
const CAPS_NONE: &str = "caps:none";

/// Which per-desktop applier owns the session. Detected from the session env only —
/// no config files, no probing binaries until a bucket is chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Desktop {
    Gnome,
    Kde,
    X11Generic,
    WaylandOther,
    Headless,
}

impl Desktop {
    /// Marker-file token (also the log name). Stable — the marker written by one run
    /// is read by a later build.
    fn token(self) -> &'static str {
        match self {
            Desktop::Gnome => "gnome",
            Desktop::Kde => "kde",
            Desktop::X11Generic => "x11",
            Desktop::WaylandOther => "wayland-other",
            Desktop::Headless => "headless",
        }
    }

    fn from_token(t: &str) -> Option<Self> {
        Some(match t {
            "gnome" => Desktop::Gnome,
            "kde" => Desktop::Kde,
            "x11" => Desktop::X11Generic,
            _ => return None,
        })
    }
}

fn detect() -> Desktop {
    // XDG_CURRENT_DESKTOP is a colon-separated list ("ubuntu:GNOME", "KDE", "sway").
    let current = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if current.split(':').any(|p| p.contains("gnome")) {
        return Desktop::Gnome;
    }
    if current
        .split(':')
        .any(|p| p.contains("kde") || p.contains("plasma"))
    {
        return Desktop::Kde;
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|s| s == "wayland")
            .unwrap_or(false);
    if wayland {
        return Desktop::WaylandOther;
    }
    if std::env::var_os("DISPLAY").is_some() {
        return Desktop::X11Generic;
    }
    Desktop::Headless
}

/// Run a command, returning trimmed stdout on success (None on spawn failure or a
/// non-zero exit — callers treat both as "this applier isn't available").
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn run_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── ownership marker ─────────────────────────────────────────────────────────

/// `$XDG_STATE_HOME/dontspeak/caps-owned` (fallback `~/.local/state/…`) — the same
/// local-state root ds-config's `Paths::state_dir` resolves on Linux, computed here
/// directly so this module stays dependency-free.
fn marker_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(state.join("dontspeak").join("caps-owned"))
}

/// Persist the ownership marker; returns whether it actually landed on disk. `own`
/// gates neutralizing the key on this succeeding — an option applied without a marker
/// is a permanent orphan (release is marker-gated), so a marker we can't write means we
/// must not touch the keymap.
fn marker_write(desktop: Desktop) -> bool {
    let Some(p) = marker_path() else {
        return false;
    };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&p, desktop.token()).is_ok()
}

fn marker_read() -> Option<Desktop> {
    let p = marker_path()?;
    let t = std::fs::read_to_string(p).ok()?;
    Desktop::from_token(t.trim())
}

fn marker_clear() {
    if let Some(p) = marker_path() {
        let _ = std::fs::remove_file(p);
    }
}

// ── per-desktop option read/write ────────────────────────────────────────────
//
// Every bucket exposes the same two primitives — current option list, replace option
// list — so own/release share one merge routine.

const GNOME_SCHEMA: &str = "org.gnome.desktop.input-sources";
const GNOME_KEY: &str = "xkb-options";

/// Parse gsettings' GVariant list output (`['a', 'b']`, or `@as []` when empty).
fn parse_gvariant_list(raw: &str) -> Vec<String> {
    let mut opts = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.find('\'') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('\'') else { break };
        opts.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    opts
}

fn gnome_options() -> Option<Vec<String>> {
    run("gsettings", &["get", GNOME_SCHEMA, GNOME_KEY]).map(|raw| parse_gvariant_list(&raw))
}

fn gnome_set_options(opts: &[String]) -> bool {
    let list = format!(
        "[{}]",
        opts.iter()
            .map(|o| format!("'{o}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    run_ok("gsettings", &["set", GNOME_SCHEMA, GNOME_KEY, &list])
}

const KXKBRC: [&str; 6] = ["--file", "kxkbrc", "--group", "Layout", "--key", "Options"];

fn kde_options() -> Option<Vec<String>> {
    // kreadconfig prints an empty line (success) when the key is absent.
    for tool in ["kreadconfig6", "kreadconfig5"] {
        if let Some(v) = run(tool, &KXKBRC) {
            return Some(split_commas(&v));
        }
    }
    None
}

fn kde_set_options(opts: &[String]) -> bool {
    let joined = opts.join(",");
    for tool in ["kwriteconfig6", "kwriteconfig5"] {
        let mut args: Vec<&str> = KXKBRC.to_vec();
        args.push(&joined);
        if run_ok(tool, &args) {
            // kxkbrc is only re-read on this session D-Bus SIGNAL; best-effort — if
            // dbus-send is missing the option still lands at next login.
            let _ = run_ok(
                "dbus-send",
                &[
                    "--session",
                    "--type=signal",
                    "/Layouts",
                    "org.kde.keyboard.reloadConfig",
                ],
            );
            return true;
        }
    }
    false
}

fn x11_options() -> Option<Vec<String>> {
    run("setxkbmap", &["-query"]).map(|q| {
        q.lines()
            .find_map(|l| l.strip_prefix("options:"))
            .map(split_commas)
            .unwrap_or_default()
    })
}

fn x11_set_options(opts: &[String]) -> bool {
    // `-option ""` resets, then each option is re-added — setxkbmap has no "remove one".
    let mut args: Vec<&str> = vec!["-option", ""];
    for o in opts {
        args.push("-option");
        args.push(o);
    }
    run_ok("setxkbmap", &args)
}

fn split_commas(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn options_for(desktop: Desktop) -> Option<Vec<String>> {
    match desktop {
        Desktop::Gnome => gnome_options(),
        Desktop::Kde => kde_options(),
        Desktop::X11Generic => x11_options(),
        _ => None,
    }
}

fn set_options_for(desktop: Desktop, opts: &[String]) -> bool {
    match desktop {
        Desktop::Gnome => gnome_set_options(opts),
        Desktop::Kde => kde_set_options(opts),
        Desktop::X11Generic => x11_set_options(opts),
        _ => false,
    }
}

// ── public API (macOS capskey.rs parity) ─────────────────────────────────────

/// Take ownership of the Caps key: add `caps:none` to the session keymap so a press
/// never toggles capitals (the physical key stays visible to the evdev monitor, and
/// the Caps LED becomes solely ours to drive). Best-effort — on any failure the key
/// falls back to normal caps-lock behavior, exactly as before.
pub fn own_caps_key() {
    // A marker from a previous run under a DIFFERENT desktop (user switched DE between
    // runs) would orphan that desktop's option — reconcile it before owning here. The
    // marker file is a SINGLE shared slot (one token, one desktop) — `release_caps_key`
    // only clears it once the strip write is CONFIRMED to have landed (or finds nothing
    // to strip); if that write fails, it deliberately leaves the marker so a later run
    // can retry. So the result must be checked here: overwriting that still-present
    // marker with the NEW desktop's token below (via `marker_write(desktop)`) would
    // orphan the prior desktop's `caps:none` for good — no marker would ever point back
    // at it again (the one shared slot now says the new desktop), so no future
    // `release_caps_key` call, reboot, or `uninstall.sh` pass could find and strip it.
    let desktop = detect();
    if let Some(prev) = marker_read()
        && prev != desktop
    {
        release_caps_key();
        if marker_read().is_some() {
            // Still there ⇒ the strip didn't land (options unreadable, or the
            // read-merge-write failed) — leave the prior desktop's marker + option
            // exactly as `release_caps_key` left them and skip owning the key under
            // the NEW desktop this run, rather than silently orphaning the old one.
            // The next launch (or a manual retry) reconciles it the same way.
            eprintln!(
                "[dontspeak] could not release the previous desktop's Caps ownership ({}); \
                 leaving it in place and skipping Caps ownership this run — will retry later",
                prev.token()
            );
            return;
        }
    }

    match desktop {
        Desktop::Gnome | Desktop::Kde | Desktop::X11Generic => {
            let Some(mut opts) = options_for(desktop) else {
                eprintln!(
                    "[dontspeak] could not read the {} keymap options; Caps will still toggle capitals",
                    desktop.token()
                );
                return;
            };
            if opts.iter().any(|o| o == CAPS_NONE) {
                // Already neutralized — by us (marker: e.g. after a hard kill) or by the
                // user's own config. Either way the key no longer toggles; change nothing.
                return;
            }
            opts.push(CAPS_NONE.to_string());
            // Marker FIRST, then the option: a marker with no option is a safe no-op on
            // release, but an option with no marker is a permanent orphan. If the marker
            // can't be persisted, leave the key as normal caps-lock rather than risk it.
            if !marker_write(desktop) {
                eprintln!(
                    "[dontspeak] could not persist the Caps ownership marker; leaving Caps as caps-lock"
                );
                return;
            }
            if set_options_for(desktop, &opts) {
                eprintln!(
                    "[dontspeak] owning Caps key ({}: added {CAPS_NONE}; no caps toggle)",
                    desktop.token()
                );
            } else {
                // The option didn't land — drop the marker we just wrote so release
                // doesn't chase a non-existent option and a later own retries cleanly.
                marker_clear();
                eprintln!(
                    "[dontspeak] could not set {CAPS_NONE} on {}; Caps will still toggle capitals",
                    desktop.token()
                );
            }
        }
        Desktop::WaylandOther => {
            eprintln!(
                "[dontspeak] this Wayland compositor's keymap can't be changed from outside — \
                 add `{CAPS_NONE}` to its config (sway: `input type:keyboard xkb_options {CAPS_NONE}`; \
                 Hyprland: `input:kb_options = {CAPS_NONE}`) to stop Caps toggling capitals; \
                 dictation works either way"
            );
        }
        Desktop::Headless => {}
    }
}

/// Release the Caps key back to the user's keymap: remove OUR `caps:none` (marker-gated,
/// so a user's own `caps:none` is never stripped).
pub fn release_caps_key() {
    let Some(desktop) = marker_read() else {
        return;
    };
    match options_for(desktop) {
        // Our option is present — strip it, and clear the marker ONLY if the write
        // lands. A failed strip that still cleared the marker would orphan a PERSISTENT
        // GNOME/KDE `caps:none` forever (a later `own` sees it marker-less, treats it as
        // the user's own, and never reclaims or releases it). Keeping the marker lets
        // the next clean run — or `uninstall.sh` — retry the strip.
        Some(opts) if opts.iter().any(|o| o == CAPS_NONE) => {
            let kept: Vec<String> = opts.into_iter().filter(|o| o != CAPS_NONE).collect();
            if set_options_for(desktop, &kept) {
                marker_clear();
            }
        }
        // Option already gone (we or the user cleared it) — nothing to strip, so the
        // marker has done its job; drop it.
        Some(_) => marker_clear(),
        // Couldn't even read the store (tool momentarily unavailable) — keep the marker
        // so a later run reconciles rather than orphaning a possibly-applied option.
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvariant_list_parses_options() {
        assert_eq!(
            parse_gvariant_list("['grp:alt_shift_toggle', 'compose:ralt']"),
            vec![
                "grp:alt_shift_toggle".to_string(),
                "compose:ralt".to_string()
            ]
        );
    }

    #[test]
    fn gvariant_empty_forms_parse_to_empty() {
        assert!(parse_gvariant_list("@as []").is_empty());
        assert!(parse_gvariant_list("[]").is_empty());
    }

    #[test]
    fn comma_split_trims_and_drops_empties() {
        assert_eq!(
            split_commas(" caps:none, grp:win_space_toggle ,,"),
            vec!["caps:none".to_string(), "grp:win_space_toggle".to_string()]
        );
    }

    #[test]
    fn marker_tokens_round_trip() {
        for d in [Desktop::Gnome, Desktop::Kde, Desktop::X11Generic] {
            assert_eq!(Desktop::from_token(d.token()), Some(d));
        }
        // Non-applier buckets never reconstruct from a marker.
        assert_eq!(Desktop::from_token("wayland-other"), None);
        assert_eq!(Desktop::from_token("headless"), None);
    }
}
