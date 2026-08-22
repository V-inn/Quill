//! One daemon at a time, enforced with an `flock`ed file.
//!
//! Two instances can genuinely happen: the udev rule
//! (`packaging/99-quill-daemon.rules`) launches the systemd user unit on every
//! Samsung USB attach, and nothing stops a hand-run `quill-daemon` alongside
//! it. When they overlap, three things break in ways that look like unrelated
//! bugs:
//!
//! - **The portal restore token is single-use.** The portal issues a fresh one
//!   per successful `Start` and invalidates the one just consumed, so whichever
//!   instance loses the race sees its token rejected and pops the screen-picker
//!   dialog at a user who may not be at the keyboard.
//! - **`orientation::ensure` runs `pkill -f krfb-virtualmonitor`**, which is
//!   process-wide and unqualified -- the second instance tears down the first
//!   instance's virtual monitor while it is streaming to it.
//! - **The AOA interface claim** is exclusive; the loser gets an opaque libusb
//!   error rather than anything that explains itself.
//!
//! `flock` is the right primitive here rather than a pid file: the kernel drops
//! the lock when the holder exits *however* it exits, including the
//! `std::process::exit(1)` paths this daemon uses everywhere, which skip
//! destructors. A stale lock file is therefore never stale.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

fn lock_path() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir).join("quill-daemon.lock"),
        // No runtime dir (a bare ssh session, say): per-uid so this can't
        // collide with another user's daemon on a shared machine.
        _ => PathBuf::from(format!("/tmp/quill-daemon-{}.lock", unsafe { libc::getuid() })),
    }
}

/// Takes the lock, or exits **0** if another daemon already holds it.
///
/// Zero, not one: `Restart=on-failure` must not restart a duplicate launch, and
/// `StartLimitBurst=3` must not be burned by one either -- burning it leaves the
/// unit permanently `failed` and needing a manual `systemctl --user
/// reset-failed`, which is how this daemon has bitten before (see `aoa.rs`'s
/// device-scan retry).
/// `O_NOFOLLOW` because of the `/tmp` fallback in `lock_path`: `/tmp` is
/// world-writable, and its sticky bit only stops others *deleting* entries, not
/// creating one at a path that doesn't exist yet. Without this, another local
/// user could pre-place a symlink at `/tmp/quill-daemon-<uid>.lock` pointing at
/// any file this uid can write, and `acquire_or_exit`'s `set_len(0)` would
/// truncate the target. 0600 for the same reason -- nobody else needs to read a
/// pid we only write for diagnostics.
///
/// Split out from `acquire_or_exit` (which takes a fixed path and exits the
/// process) so the symlink refusal is actually testable -- see below.
fn open_lock_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(path)
}

pub fn acquire_or_exit() {
    let path = lock_path();
    let mut file = match open_lock_file(&path) {
        Ok(f) => f,
        Err(e) => {
            // Not fatal: a daemon that can't create its lock file is still a
            // working daemon, just an unguarded one.
            if e.raw_os_error() == Some(libc::ELOOP) {
                eprintln!(
                    "[lock] {} is a symlink -- refusing to follow it (see the O_NOFOLLOW note above); \
                     continuing without a single-instance guard",
                    path.display()
                );
            } else {
                eprintln!("[lock] can't open {} ({e}) -- continuing without a single-instance guard", path.display());
            }
            return;
        }
    };

    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock {
            eprintln!("[lock] flock on {} failed ({err}) -- continuing without a single-instance guard", path.display());
            return;
        }
        let mut holder = String::new();
        let _ = file.read_to_string(&mut holder);
        let holder = holder.trim();
        if holder.is_empty() {
            eprintln!("[lock] another quill daemon is already running -- exiting");
        } else {
            eprintln!("[lock] another quill daemon is already running (pid {holder}) -- exiting");
        }
        std::process::exit(0);
    }

    let _ = file.set_len(0);
    let _ = write!(file, "{}", std::process::id());
    let _ = file.flush();
    // The lock lives as long as the fd does, and this daemon exits from several
    // places that skip `Drop` -- so hand the fd to the process rather than to a
    // value whose lifetime we would then have to thread through `main`.
    std::mem::forget(file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The `/tmp`-fallback attack, end to end: a symlink is planted at the lock
    /// path pointing at a file this uid owns, and the open must refuse rather
    /// than truncate what's on the other end.
    #[test]
    fn refuses_to_follow_a_symlink_and_leaves_the_target_intact() {
        let dir = std::env::temp_dir().join(format!("quill-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let victim = dir.join("victim");
        std::fs::write(&victim, "precious").unwrap();
        let lock = dir.join("quill-daemon.lock");
        std::os::unix::fs::symlink(&victim, &lock).unwrap();

        let err = open_lock_file(&lock).expect_err("must not follow the symlink");
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));

        let after = std::fs::read_to_string(&victim).unwrap();
        assert_eq!(after, "precious", "target was modified through the symlink");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The ordinary path still works, and creates the file 0600.
    #[test]
    fn creates_a_private_lock_file_on_the_normal_path() {
        let dir = std::env::temp_dir().join(format!("quill-lock-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let lock = dir.join("quill-daemon.lock");
        let file = open_lock_file(&lock).expect("plain path must open");
        let mode = file.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "lock file should not be readable by others");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
