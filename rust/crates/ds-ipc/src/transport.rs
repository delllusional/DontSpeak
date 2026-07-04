//! Transport seam for the RPC plane. Both ends (`server`, `client`) speak
//! newline-delimited JSON over a byte stream supporting `Read`, `Write`,
//! `try_clone()`, `set_read_timeout` and `set_write_timeout`. This module is the
//! ONLY place that names the concrete OS transport, so the framing/timeout logic
//! above it is platform-agnostic.
//!
//! - unix (macOS/Linux): a filesystem Unix-domain socket — the current, shipping
//!   transport, kept byte-identical (stale-socket unlink on bind, same accept
//!   loop, same timeouts set by the client).
//! - windows: a real AF_UNIX filesystem socket via the `uds_windows` crate
//!   (Win10 1803+ ships AF_UNIX; std just doesn't expose it). Its
//!   `UnixListener`/`UnixStream` mirror std's surface, so this arm is a
//!   near-verbatim copy of the unix one and nothing above this module changes.
//!   SECURITY: same filesystem-scoped `.sock` model as unix — the socket lives
//!   under `ds_config::Paths::state_dir` (`engine_sock`), which varies per OS
//!   (`$XDG_STATE_HOME/dontspeak` on Linux, `%LOCALAPPDATA%\DontSpeak` on
//!   Windows, `~/Library/Application Support/DontSpeak` on macOS — see
//!   `ds-config/src/paths.rs::state_root`) and is deliberately *not*
//!   `~/.claude`, which is Claude Code's own tree and stays untouched. No
//!   loopback-TCP + auth-token handshake is needed (which is why the earlier
//!   TCP design was dropped) because both arms of `bind()` below now
//!   explicitly enforce restrictive permissions rather than relying on the
//!   process umask: 0700 on the socket's parent dir + 0600 on the socket file
//!   itself on Unix, and an explicit owner+SYSTEM-only DACL on both, on
//!   Windows.

use std::io;
use std::path::Path;

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;

    /// The connected byte stream (one per client connection).
    pub type Stream = UnixStream;
    /// The accepting endpoint owned by the server.
    pub type Listener = UnixListener;

    /// Bind the server endpoint at `path`. Removes a stale socket file from a
    /// crashed run first (so a restart never fails with `EADDRINUSE`), creates
    /// the parent dir, and explicitly hardens permissions: 0700 on the parent
    /// dir (set before the socket file is created inside it, so no other local
    /// user can even traverse into the dir during the gap before the file-level
    /// chmod below runs) and 0600 on the socket file itself (belt-and-suspenders,
    /// since Linux also enforces the file's own mode on `connect()`).
    pub fn bind(path: &Path) -> io::Result<Listener> {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    /// Connect a client stream to the server endpoint at `path`.
    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path)
    }
}

#[cfg(windows)]
mod imp {
    // Real AF_UNIX filesystem socket via `uds_windows` — a near-verbatim mirror of
    // the unix arm (same stale-socket unlink on bind, same parent-dir creation).
    use std::io;
    use std::path::Path;
    use uds_windows::{UnixListener, UnixStream};
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };
    use windows::core::{HSTRING, PCWSTR};

    /// The connected byte stream (one per client connection).
    pub type Stream = UnixStream;
    /// The accepting endpoint owned by the server.
    pub type Listener = UnixListener;

    /// Harden `path`'s ACL to owner + SYSTEM only, with a protected (non-inherited)
    /// DACL — NTFS has no chmod-bit model, so this is the Windows analogue of the
    /// Unix side's `0700`/`0600`: an explicit, non-inherited grant to the object
    /// owner and the SYSTEM account, nobody else (deliberately excluding
    /// `BUILTIN\Administrators`, to match Unix's "only the owner, not even other
    /// privileged accounts get an implicit grant" semantics).
    fn harden(path: &Path) -> io::Result<()> {
        // SAFETY: `sddl` is a valid NUL-terminated wide string for the lifetime of
        // the call; `sd` is populated by the API and freed via `LocalFree` below
        // once `SetFileSecurityW` has consumed it.
        unsafe {
            let sddl = HSTRING::from("D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)");
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .map_err(|_| io::Error::last_os_error())?;

            let file = HSTRING::from(path.as_os_str());
            let ok = SetFileSecurityW(
                PCWSTR(file.as_ptr()),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                sd,
            );
            let err = if ok.as_bool() {
                None
            } else {
                Some(io::Error::last_os_error())
            };
            let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(sd.0)));
            if let Some(err) = err {
                return Err(err);
            }
        }
        Ok(())
    }

    /// Bind the server endpoint at `path`. Removes a stale socket file from a
    /// crashed run first (AF_UNIX bind fails if the path exists), creates the
    /// parent dir, and explicitly hardens permissions: an owner+SYSTEM-only,
    /// non-inherited DACL on the parent dir (set before the socket file is
    /// created inside it — the file inherits the restrictive ACL via NTFS
    /// `OICI` inheritance since it's created afterward) and again on the socket
    /// file itself (defense-in-depth, mirroring the Unix side's belt-and-
    /// suspenders file-level chmod).
    pub fn bind(path: &Path) -> io::Result<Listener> {
        let _ = std::fs::remove_file(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            harden(dir)?;
        }
        let listener = UnixListener::bind(path)?;
        harden(path)?;
        Ok(listener)
    }

    /// Connect a client stream to the server endpoint at `path`.
    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path)
    }
}

pub use imp::{Listener, Stream};

/// Bind the server endpoint at `path` (see backend docs).
pub fn bind(path: &Path) -> io::Result<Listener> {
    imp::bind(path)
}

/// Connect a client stream to the server endpoint at `path`.
pub fn connect(path: &Path) -> io::Result<Stream> {
    imp::connect(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn bind_hardens_dir_and_socket_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("dontspeak.sock");
        let _listener = bind(&path).expect("bind");

        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .expect("dir metadata")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);

        let file_mode = std::fs::metadata(&path)
            .expect("socket metadata")
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o777, 0o600);
    }

    #[cfg(windows)]
    #[test]
    fn bind_hardens_socket_acl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dontspeak.sock");
        let _listener = bind(&path).expect("bind");

        let output = std::process::Command::new("icacls")
            .arg(&path)
            .output()
            .expect("run icacls");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("Everyone")
                && !stdout.contains("BUILTIN\\Users")
                && !stdout.contains("Authenticated Users"),
            "icacls output granted access to an unexpected principal: {stdout}"
        );
    }
}
