//! ONE renderer/recogniser for hook COMMAND STRINGS (Codex, Qwen InlineShell, Kimi Code).
//! Claude Code uses argv (`ArgsArray`) and never comes here — only client whose Windows
//! hooks worked before this module.
//!
//! ## Windows: no double quotes in the command string
//!
//! Runners pass the whole string as ONE argv to cmd (`/C`). Rust/Node escape `"` as `\"`
//! per `CommandLineToArgvW`; **cmd.exe does not** and treats `\"` literally — so a quoted
//! path becomes a non-existent program name. Windows forms here are quote-free; spaced
//! paths use other means (see `ShellOverride` / short names).

/// OS dialect for the command string. A parameter (not `cfg!`) so BOTH forms are unit-tested
/// on Linux CI; production selects via [`host_inline_flavor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InlineFlavor {
    /// `bash -c` / `sh -lc` — quotes parse correctly.
    Unix,
    /// `%ComSpec%` (`cmd /C`), Git Bash `-c` under MSYS/MinGW, or PowerShell if ComSpec repointed.
    Windows,
}

/// Can this client's hook schema carry a per-hook shell override?
///
/// Only axis among string-runner clients; decides the one case a quote-free string cannot
/// express: a bin path containing a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellOverride {
    /// Qwen: `shell` field exists → pin spaced path to PowerShell (`\"` parses).
    Supported,
    /// Codex: no shell field → spaced path must be space-FREE (8.3 short name).
    Unsupported,
}

/// Production host dialect; tests pass both flavors explicitly so Linux CI covers Windows form.
pub(crate) fn host_inline_flavor() -> InlineFlavor {
    if cfg!(windows) {
        InlineFlavor::Windows
    } else {
        InlineFlavor::Unix
    }
}

/// Render hook verbs into one shell command. Returns `(command, per-hook shell override)`;
/// override is always `None` for [`ShellOverride::Unsupported`].
///
///   * **Unix** — `"<bin>" <verbs>` (always quoted; correct for spaced paths).
///   * **Windows, spaceless bin** (normal install) — forward-slash, unquoted, no override.
///     Shell-agnostic (cmd/PowerShell/Git Bash); avoids PowerShell startup on per-prompt `provide`.
///   * **Windows, spaced bin** — no quote-free string works, and quotes cannot survive cmd:
///       - [`ShellOverride::Supported`] → `& "<bin>" <verbs>` + `"shell": "powershell"`.
///       - [`ShellOverride::Unsupported`] → 8.3 short name (space-free).
///
/// If 8.3 generation is disabled, `GetShortPathNameW` returns the long path unchanged and
/// there is no representable command: emit QUOTED form (works under Git Bash/PowerShell;
/// never emit unquoted-spaced, which is broken under every shell).
pub(crate) fn inline_command<I, S>(
    flavor: InlineFlavor,
    bin: &str,
    verbs: I,
    shell_override: ShellOverride,
) -> (String, Option<&'static str>)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    inline_command_with(flavor, bin, verbs, shell_override, host_short_path)
}

/// Injectable core — `shorten` is the 8.3 lookup so Windows spaced-path branches run on Linux CI.
fn inline_command_with<I, S>(
    flavor: InlineFlavor,
    bin: &str,
    verbs: I,
    shell_override: ShellOverride,
    shorten: impl Fn(&str) -> Option<String>,
) -> (String, Option<&'static str>)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut verbs_text = String::new();
    for verb in verbs {
        if !verbs_text.is_empty() {
            verbs_text.push(' ');
        }
        verbs_text.push_str(verb.as_ref());
    }
    let verbs = verbs_text;
    let unquoted = |p: &str| (format!("{} {verbs}", p.replace('\\', "/")), None);
    let quoted = || (format!("\"{bin}\" {verbs}"), None);

    match flavor {
        InlineFlavor::Unix => quoted(),
        InlineFlavor::Windows if !bin.contains(char::is_whitespace) => unquoted(bin),
        InlineFlavor::Windows => match shell_override {
            ShellOverride::Supported => (format!("& \"{bin}\" {verbs}"), Some("powershell")),
            // 8.3 only if it actually removed spaces (disabled volume hands long path back).
            ShellOverride::Unsupported => shorten(bin)
                .filter(|s| !s.contains(char::is_whitespace))
                .map_or_else(quoted, |short| unquoted(&short)),
        },
    }
}

/// 8.3 short name, or `None` if unavailable / off Windows. Declared against `kernel32` (MSVC
/// links by default — no new dep).
#[cfg(windows)]
fn host_short_path(long: &str) -> Option<String> {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    unsafe extern "system" {
        fn GetShortPathNameW(long_path: *const u16, short_path: *mut u16, buf_len: u32) -> u32;
    }

    let wide: Vec<u16> = OsStr::new(long)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Null buffer → size REQUIRED including NUL; 0 = failure.
    //
    // SAFETY: `wide` is NUL-terminated UTF-16 and outlives the call. Null out + zero length is
    // the documented size query — Win32 writes nothing.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    // Second call returns length WITHOUT NUL; 0 or ≥ needed = failure / path changed.
    //
    // SAFETY: same input; `buf` is `needed` u16s with matching capacity — no OOB write.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed) };
    if written == 0 || written as usize >= needed as usize {
        return None;
    }
    Some(
        OsString::from_wide(&buf[..written as usize])
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(not(windows))]
fn host_short_path(_long: &str) -> Option<String> {
    None
}

/// Is this wired `command` OURS — the `dontspeak` binary, in ANY dialect we have ever written?
/// Shared so re-wire self-heals (old dialect still recognised → merge REPLACES, not duplicates)
/// and unwire still removes it.
///
/// Accepts bare path AND every inlined form. OS-separator-independent parse (`Path` won't split
/// `\` on Linux; CI runs Windows-flavor tests): trim → strip optional leading `&` → path token
/// (quoted span or up to whitespace) → basename → stem → `== "dontspeak"`.
///
/// Precision both ways: a user command that merely has `dontspeak` in its path must not match —
/// would silently skip wiring AND make unwire delete the user's hook group.
pub(crate) fn command_is_ours(cmd: &str) -> bool {
    let s = cmd.trim();
    let s = s.strip_prefix('&').map(str::trim_start).unwrap_or(s);
    let path = match s.strip_prefix('"') {
        // Unterminated quote takes the rest (defensive — we never write one).
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => s.split_whitespace().next().unwrap_or(""),
    };
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base, // no extension, or leading-dot name
    };
    stem == "dontspeak"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_shorten(long: &str) -> Option<String> {
        Some(long.replace("Alex Smith", "ALEXSM~1"))
    }

    fn no_shorten(long: &str) -> Option<String> {
        Some(long.to_string())
    }

    #[test]
    fn unix_always_quotes_the_path() {
        for style in [ShellOverride::Supported, ShellOverride::Unsupported] {
            assert_eq!(
                inline_command(InlineFlavor::Unix, "/bin/dontspeak", ["notify"], style),
                ("\"/bin/dontspeak\" notify".to_string(), None)
            );
            assert_eq!(
                inline_command(
                    InlineFlavor::Unix,
                    "/opt/x y/dontspeak",
                    ["notify", "--greet-only"],
                    style
                ),
                (
                    "\"/opt/x y/dontspeak\" notify --greet-only".to_string(),
                    None
                )
            );
        }
    }

    #[test]
    fn windows_spaceless_path_is_unquoted_for_every_string_runner_client() {
        // REGRESSION: a double quote in a Windows command string IS the bug — cmd gets
        // `\"` literally and exits 1. Normal install path has no spaces.
        let bin = r"C:\Users\usr\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        for style in [ShellOverride::Supported, ShellOverride::Unsupported] {
            let (cmd, shell) = inline_command(InlineFlavor::Windows, bin, ["notify"], style);
            assert_eq!(
                cmd,
                "C:/Users/usr/AppData/Local/Programs/DontSpeak/dontspeak.exe notify"
            );
            assert!(!cmd.contains('"'), "no quote may reach cmd.exe: {cmd}");
            assert_eq!(shell, None);

            // `--client <token>` is snake_case — space-joining cannot reintroduce a quote.
            let (cmd, shell) = inline_command(
                InlineFlavor::Windows,
                bin,
                ["notify", "--client", "codex"],
                style,
            );
            assert_eq!(
                cmd,
                "C:/Users/usr/AppData/Local/Programs/DontSpeak/dontspeak.exe notify --client codex"
            );
            assert!(!cmd.contains('"'), "no quote may reach cmd.exe: {cmd}");
            assert_eq!(shell, None);
        }
    }

    #[test]
    fn windows_spaced_path_with_shell_override_pins_powershell() {
        let bin = r"C:\Users\Alex Smith\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        assert_eq!(
            inline_command(
                InlineFlavor::Windows,
                bin,
                ["provide"],
                ShellOverride::Supported
            ),
            (format!("& \"{bin}\" provide"), Some("powershell"))
        );
    }

    #[test]
    fn windows_spaced_path_without_shell_override_uses_the_8dot3_short_name() {
        let bin = r"C:\Users\Alex Smith\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        let (cmd, shell) = inline_command_with(
            InlineFlavor::Windows,
            bin,
            ["notify"],
            ShellOverride::Unsupported,
            fake_shorten,
        );
        assert_eq!(
            cmd,
            "C:/Users/ALEXSM~1/AppData/Local/Programs/DontSpeak/dontspeak.exe notify"
        );
        assert!(!cmd.contains('"'), "no quote may reach cmd.exe: {cmd}");
        assert!(!cmd.contains("Alex Smith"));
        assert_eq!(shell, None);
    }

    #[test]
    fn windows_spaced_path_with_8dot3_disabled_emits_quoted_not_a_bare_spaced_path() {
        // 8.3 disabled ⇒ quoted form (broken under cmd; OK under Bash/PowerShell).
        // Never emit unquoted spaced path.
        let bin = r"C:\Users\Alex Smith\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        let (cmd, shell) = inline_command_with(
            InlineFlavor::Windows,
            bin,
            ["notify"],
            ShellOverride::Unsupported,
            no_shorten,
        );
        assert_eq!(cmd, format!("\"{bin}\" notify"));
        assert_eq!(shell, None);
    }

    #[test]
    fn command_is_ours_accepts_every_dialect_we_write_and_rejects_the_rest() {
        assert!(command_is_ours("/bin/dontspeak"));
        assert!(command_is_ours(
            r"C:\Users\usr\AppData\Local\Programs\DontSpeak\dontspeak.exe"
        ));
        assert!(command_is_ours("\"/opt/x y/dontspeak\" notify"));
        assert!(command_is_ours(
            "C:/Users/usr/AppData/Local/Programs/DontSpeak/dontspeak.exe notify --greet-only"
        ));
        assert!(command_is_ours(
            "& \"C:\\Users\\Alex Smith\\DontSpeak\\dontspeak.exe\" provide"
        ));
        assert!(command_is_ours(
            "C:/Users/ALEXSM~1/AppData/Local/Programs/DontSpeak/dontspeak.exe notify"
        ));
        // Recognition keys on leading path only — trailing verbs don't affect it (pre-token
        // groups still heal).
        assert!(command_is_ours(
            "\"/opt/x y/dontspeak\" notify --greet-only --client qwen_code"
        ));
        assert!(command_is_ours(
            "C:/Users/usr/AppData/Local/Programs/DontSpeak/dontspeak.exe provide --client codex"
        ));
        assert!(!command_is_ours("/usr/bin/true"));
        assert!(!command_is_ours("dontspeak-uninstall"));
        assert!(!command_is_ours("ds-sync"));
        assert!(!command_is_ours("/home/me/dontspeak/my-own-script.sh"));
        assert!(!command_is_ours(""));
    }
}
