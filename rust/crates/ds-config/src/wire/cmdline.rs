//! The ONE renderer + recogniser for DontSpeak's hook COMMAND STRING — shared by every
//! client whose hook runner is handed a single command *string* instead of an argv array:
//! Codex (`ClaudeTomlHooks`), Grok (`GrokJsonHooks`) and Qwen Code (`ClaudeJsonHooks` +
//! `HookCommandStyle::InlineShell`).
//!
//! Claude Code is the ONE client that never comes here: it takes `command` + an `args` array
//! and spawns them DIRECTLY, with no shell in between (`HookCommandStyle::ArgsArray`), so
//! nothing can re-quote or re-parse its path. That is exactly why it was the only client
//! whose Windows hooks ever worked — see the quoting note below.
//!
//! ## Why a Windows command string must not contain double quotes
//!
//! Every one of these runners spawns its shell with the whole command string as ONE argv
//! element: Codex does `Command::new($COMSPEC).arg("/C").arg(command)`
//! (`codex-rs/hooks/src/engine/command_runner.rs`), Qwen does
//! `spawn(comspec, [...prefix, command], {shell:false})`. Both Rust and Node then escape that
//! element per `CommandLineToArgvW` rules, which rewrite an embedded `"` as `\"`. **cmd.exe
//! does not implement those rules** — it has its own quote handling and treats `\"`
//! literally. So `"C:\…\dontspeak.exe" notify` reaches cmd as `\"C:\…\dontspeak.exe\" notify`
//! and cmd tries to execute a program literally named `\"C:\…\dontspeak.exe\"`:
//!
//! ```text
//! '\"C:\Users\usr\AppData\Local\Programs\DontSpeak\dontspeak.exe\"' is not recognized
//! as an internal or external command, operable program or batch file.
//! ```
//!
//! …exiting 1, before our binary ever runs. Reproduced against both runners. Hence the
//! Windows forms below are quote-free, and a spaced path is handled by other means.

/// The OS command-line dialect a hook command string is formatted for. A parameter (not
/// `cfg!` inside the formatter) so BOTH forms are unit-tested on Linux CI; production selects
/// via [`host_inline_flavor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InlineFlavor {
    /// POSIX hosts: the hook shell is `bash -c` / `sh -lc`, which parse quotes correctly.
    Unix,
    /// Windows: the hook shell is `%ComSpec%` (`cmd /C`, or `/d /s /c`) by default, Git Bash
    /// `-c` under MSYS/MinGW, or `powershell -NoProfile -Command` if ComSpec is repointed.
    Windows,
}

/// Can this client's hook schema carry a PER-HOOK shell override alongside the command?
///
/// The only axis on which the three string-runner clients differ, and it decides the one case
/// a quote-free string cannot express: a bin path containing a space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellOverride {
    /// Qwen Code: its `CommandHookConfig` has a `shell` field, so a spaced-path command can be
    /// pinned to PowerShell — which does parse the `\"`-escaped argv Node emits.
    Supported,
    /// Codex and Grok: their command-hook schemas carry `command` (plus Codex's
    /// `command_windows` / `timeout` / `async` / `statusMessage`) and nothing else — there is
    /// NO shell field, so a spaced path must be made space-FREE instead (see
    /// [`inline_command`]).
    Unsupported,
}

/// The inline dialect for THIS host OS — production callers use this; tests drive
/// [`inline_command`] with both flavors explicitly so Linux CI covers the Windows form too.
pub(crate) fn host_inline_flavor() -> InlineFlavor {
    if cfg!(windows) {
        InlineFlavor::Windows
    } else {
        InlineFlavor::Unix
    }
}

/// Render the hook verbs into ONE shell command string. Returns `(command, per-hook shell
/// override)`; the override is always `None` for a [`ShellOverride::Unsupported`] client.
///
///   * **Unix** — `"<bin>" <verbs>`, path always double-quoted. Correct for spaced paths.
///   * **Windows, spaceless bin path** (the normal `%LOCALAPPDATA%\Programs\DontSpeak` case) —
///     forward-slash-normalised, UNQUOTED, no shell override. Runs under all three possible
///     hook shells (cmd, PowerShell, Git Bash), so the common case is shell-agnostic and free
///     of PowerShell's startup tax on the synchronous per-prompt `provide`.
///   * **Windows, spaced bin path** (e.g. a username with a space) — no quote-free string can
///     name it, and quotes cannot survive cmd (see module docs), so:
///       - [`ShellOverride::Supported`] (Qwen) → `& "<bin>" <verbs>` plus `"shell":
///         "powershell"`, pinning the runner to the one shell that parses that escaping.
///       - [`ShellOverride::Unsupported`] (Codex, Grok) → the path's 8.3 SHORT name
///         (`C:\Users\ALEXSM~1\…`), which is space-free and therefore needs no quotes.
///
/// If 8.3 generation is disabled on the volume (`fsutil 8dot3name`), `GetShortPathNameW`
/// returns the long path unchanged and there is NO representable command: an unquoted spaced
/// path is broken under every shell, a quoted one is broken under cmd specifically. We emit
/// the QUOTED form there — correct for a Git Bash / PowerShell-hosted runner, and never the
/// unquoted-spaced string that is broken everywhere.
pub(crate) fn inline_command(
    flavor: InlineFlavor,
    bin: &str,
    verbs: &[&str],
    shell_override: ShellOverride,
) -> (String, Option<&'static str>) {
    inline_command_with(flavor, bin, verbs, shell_override, host_short_path)
}

/// Injectable core of [`inline_command`] — `shorten` is the 8.3 lookup, so the Windows
/// spaced-path branches are exercised on Linux CI (where no Win32 call exists) via a stub.
fn inline_command_with(
    flavor: InlineFlavor,
    bin: &str,
    verbs: &[&str],
    shell_override: ShellOverride,
    shorten: impl Fn(&str) -> Option<String>,
) -> (String, Option<&'static str>) {
    let verbs = verbs.join(" ");
    let unquoted = |p: &str| (format!("{} {verbs}", p.replace('\\', "/")), None);
    let quoted = || (format!("\"{bin}\" {verbs}"), None);

    match flavor {
        InlineFlavor::Unix => quoted(),
        InlineFlavor::Windows if !bin.contains(char::is_whitespace) => unquoted(bin),
        InlineFlavor::Windows => match shell_override {
            ShellOverride::Supported => (format!("& \"{bin}\" {verbs}"), Some("powershell")),
            // The 8.3 name, but only if it actually removed the spaces — a volume with 8.3
            // generation disabled hands the long path straight back.
            ShellOverride::Unsupported => shorten(bin)
                .filter(|s| !s.contains(char::is_whitespace))
                .map_or_else(quoted, |short| unquoted(&short)),
        },
    }
}

/// The path's 8.3 short name, or `None` if Windows can't produce one (8.3 disabled on the
/// volume, path missing) — and always `None` off Windows. Declared straight against
/// `kernel32`, which MSVC targets link by default, so this needs no new dependency.
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
    // With a null buffer, GetShortPathNameW returns the size REQUIRED including the NUL; 0 is
    // failure (no 8.3 name, path absent, …).
    //
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call. Passing a null
    // output pointer with a zero length is the documented "how big a buffer do I need?" query
    // — Win32 writes nothing in that mode.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    // The second call returns the length WITHOUT the NUL; 0 is failure, and anything at or
    // past `needed` means the path changed under us.
    //
    // SAFETY: same NUL-terminated input; `buf` is a live allocation of exactly `needed` u16s
    // and we hand Win32 that same `needed` as the capacity, so it cannot write out of bounds.
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

/// Is this wired `command` string OURS — the single `dontspeak` binary, in ANY dialect we have
/// ever written? Shared by every client so a re-wire SELF-HEALS: an entry left by an older
/// dialect (the pre-fix quoted `"<bin>" verb` form, say) is still recognised as ours, so merge
/// REPLACES it instead of appending a duplicate, and unwire still removes it.
///
/// Accepts the bare binary path (args-array style) AND every inlined shell form (`"<bin>"
/// notify`, `C:/…/dontspeak.exe notify --greet-only`, `& "C:\…\dontspeak.exe" provide`). A
/// manual, OS-separator-INDEPENDENT parse — `std::path::Path` won't split `\` on Linux, and
/// Linux CI runs the Windows-flavor tests — so: trim → strip an optional leading `&` (the
/// PowerShell call operator) → take the path token (a quoted span if it opens with `"`, else
/// up to the first whitespace) → basename after the last `/` or `\` → stem (one final `.ext`
/// stripped) → `== "dontspeak"`.
///
/// Precision matters in BOTH directions: a user's own command that merely has `dontspeak`
/// somewhere in its path must not be misidentified as ours — that was empirically reproduced
/// to silently skip wiring our real hook (merge sees the event as already ours) AND to make
/// unwire delete the user's entire hook group.
pub(crate) fn command_is_ours(cmd: &str) -> bool {
    let s = cmd.trim();
    let s = s.strip_prefix('&').map(str::trim_start).unwrap_or(s);
    let path = match s.strip_prefix('"') {
        // Quoted span; an unterminated quote takes the rest (defensive — we never write one).
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => s.split_whitespace().next().unwrap_or(""),
    };
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base, // no extension, or a leading-dot name — the whole base is the stem
    };
    stem == "dontspeak"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub 8.3 lookup: mimics Windows collapsing a spaced component to `NAME~1`.
    fn fake_shorten(long: &str) -> Option<String> {
        Some(long.replace("Alex Smith", "ALEXSM~1"))
    }

    /// A volume with 8.3 generation DISABLED: GetShortPathNameW hands the long path back.
    fn no_shorten(long: &str) -> Option<String> {
        Some(long.to_string())
    }

    #[test]
    fn unix_always_quotes_the_path() {
        for style in [ShellOverride::Supported, ShellOverride::Unsupported] {
            assert_eq!(
                inline_command(InlineFlavor::Unix, "/bin/dontspeak", &["notify"], style),
                ("\"/bin/dontspeak\" notify".to_string(), None)
            );
            assert_eq!(
                inline_command(
                    InlineFlavor::Unix,
                    "/opt/x y/dontspeak",
                    &["notify", "--greet-only"],
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
        // THE REGRESSION GUARD. A double quote anywhere in a Windows command string IS the
        // bug: cmd.exe gets Rust/Node's `\"` escaping literally and exits 1 before our binary
        // runs. The normal install path has no spaces, so this is the form nearly every
        // Windows user gets — identical for all three string-runner clients.
        let bin = r"C:\Users\usr\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        for style in [ShellOverride::Supported, ShellOverride::Unsupported] {
            let (cmd, shell) = inline_command(InlineFlavor::Windows, bin, &["notify"], style);
            assert_eq!(
                cmd,
                "C:/Users/usr/AppData/Local/Programs/DontSpeak/dontspeak.exe notify"
            );
            assert!(!cmd.contains('"'), "no quote may reach cmd.exe: {cmd}");
            assert_eq!(shell, None);
        }
    }

    #[test]
    fn windows_spaced_path_with_shell_override_pins_powershell() {
        // Qwen can carry a `shell` field, so a spaced path rides PowerShell — the one Windows
        // shell that parses the `\"`-escaped argv Node emits.
        let bin = r"C:\Users\Alex Smith\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        assert_eq!(
            inline_command(
                InlineFlavor::Windows,
                bin,
                &["provide"],
                ShellOverride::Supported
            ),
            (format!("& \"{bin}\" provide"), Some("powershell"))
        );
    }

    #[test]
    fn windows_spaced_path_without_shell_override_uses_the_8dot3_short_name() {
        // Codex/Grok have no `shell` field, so the only quote-free way to name a spaced path
        // is its 8.3 short name.
        let bin = r"C:\Users\Alex Smith\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        let (cmd, shell) = inline_command_with(
            InlineFlavor::Windows,
            bin,
            &["notify"],
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
        // 8.3 disabled ⇒ no representable command. Emit the QUOTED form: broken under cmd, but
        // correct under a Git Bash / PowerShell-hosted runner. The unquoted spaced path we must
        // never emit is broken under EVERY shell.
        let bin = r"C:\Users\Alex Smith\AppData\Local\Programs\DontSpeak\dontspeak.exe";
        let (cmd, shell) = inline_command_with(
            InlineFlavor::Windows,
            bin,
            &["notify"],
            ShellOverride::Unsupported,
            no_shorten,
        );
        assert_eq!(cmd, format!("\"{bin}\" notify"));
        assert_eq!(shell, None);
    }

    #[test]
    fn command_is_ours_accepts_every_dialect_we_write_and_rejects_the_rest() {
        // Bare (args-array), Unix-quoted, Windows-unquoted, PowerShell call-operator.
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
        // The 8.3 form a spaced-path Codex/Grok wiring now emits.
        assert!(command_is_ours(
            "C:/Users/ALEXSM~1/AppData/Local/Programs/DontSpeak/dontspeak.exe notify"
        ));
        // Not ours: a different binary, or merely a `dontspeak` PATH COMPONENT.
        assert!(!command_is_ours("/usr/bin/true"));
        assert!(!command_is_ours("dontspeak-uninstall"));
        assert!(!command_is_ours("ds-sync"));
        assert!(!command_is_ours("/home/me/dontspeak/my-own-script.sh"));
        assert!(!command_is_ours(""));
    }
}
