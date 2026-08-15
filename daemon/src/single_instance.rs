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
pub fn acquire_or_exit() {
    let path = lock_path();
    let mut file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path) {
        Ok(f) => f,
        Err(e) => {
            // Not fatal: a daemon that can't create its lock file is still a
            // working daemon, just an unguarded one.
            eprintln!("[lock] can't open {} ({e}) -- continuing without a single-instance guard", path.display());
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
