//! Transport seam for the RPC plane. Server/client speak NDJSON over a byte stream with
//! `Read`/`Write`/`try_clone`/`set_{read,write}_timeout`. Only place that names the OS
//! transport — framing/timeouts above stay platform-agnostic.
//!
//! - **unix:** filesystem UDS; stale-socket unlink on bind; same accept loop/timeouts.
//! - **windows:** AF_UNIX via `uds_windows` (Win10 1803+; std lacks it). Near-verbatim of unix.
//!
//! SECURITY: socket under `ds_config::Paths::state_dir` (`engine_sock`) — not `~/.claude`.
//! Both `bind()` arms enforce restrictive permissions (not umask-dependent): Unix 0700 parent
//! and 0600 socket; Windows owner+SYSTEM-only protected DACL on both. Earlier TCP+auth design
//! dropped once UDS ACL hardening landed.

use std::io;
use std::path::Path;

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;

    pub type Stream = UnixStream;
    pub type Listener = UnixListener;

    /// Bind at `path`: unlink stale socket, create parent, harden 0700 dir / 0600 file.
    /// Dir mode set before the socket exists so no other user can traverse during the gap;
    /// file mode is belt-and-suspenders (Linux also checks the file mode on `connect()`).
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

    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path)
    }
}

#[cfg(windows)]
mod imp {
    // AF_UNIX via `uds_windows` — same stale unlink + parent-dir creation as unix.
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

    pub type Stream = UnixStream;
    pub type Listener = UnixListener;

    /// Owner+SYSTEM only, protected (non-inherited) DACL — Windows analogue of Unix 0700/0600.
    /// Deliberately excludes `BUILTIN\Administrators` to match Unix "owner only" semantics.
    fn harden(path: &Path) -> io::Result<()> {
        // SAFETY: `sddl` is a valid NUL-terminated wide string for the lifetime of the call;
        // `sd` is populated by the API and freed via `LocalFree` after `SetFileSecurityW`.
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

    /// Bind: unlink stale path, harden parent (before create — OICI inheritance), harden socket.
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

    pub fn connect(path: &Path) -> io::Result<Stream> {
        UnixStream::connect(path)
    }
}

pub use imp::{Listener, Stream};

pub fn bind(path: &Path) -> io::Result<Listener> {
    imp::bind(path)
}

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
