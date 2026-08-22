//! Which compositor this daemon is talking to, and the one shared helper for
//! running a D-Bus call from a plain synchronous caller.
//!
//! Everything KDE-specific in this project funnels through two shell-outs
//! (`krfb-virtualmonitor` to create the virtual output, `kscreen-doctor -j` to
//! read the desktop layout back). Neither exists on GNOME, and neither has a
//! drop-in GNOME equivalent -- mutter's answers are D-Bus interfaces, not
//! command-line tools (see `gnome_screencast.rs` and `gnome_display.rs`). So
//! the split is a real fork in the road at three call sites, not a config
//! knob, and this is what picks the branch.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// KWin: `krfb-virtualmonitor` for the output, the xdg ScreenCast portal
    /// (with its picker dialog and restore token) for the pixels.
    Kde,
    /// Mutter: `org.gnome.Mutter.ScreenCast.RecordVirtual`, which creates the
    /// virtual output *and* hands back the PipeWire node in one call, with no
    /// portal, no dialog, and no restore token anywhere in the picture.
    Gnome,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Kde => "KDE/KWin",
            Backend::Gnome => "GNOME/mutter",
        }
    }
}

/// Detected once and cached: this is read from three different threads (main,
/// the input thread's lazy layout resolve, the capture loop) and the answer
/// cannot change while the process is alive.
static BACKEND: OnceLock<Backend> = OnceLock::new();

pub fn backend() -> Backend {
    *BACKEND.get_or_init(detect)
}

/// `XDG_CURRENT_DESKTOP` is a colon-separated *list* ("ubuntu:GNOME",
/// "KDE", "GNOME-Classic:GNOME"), so this matches components rather than the
/// whole string.
fn from_desktop_list(value: &str) -> Option<Backend> {
    for part in value.split(':') {
        let part = part.trim();
        if part.eq_ignore_ascii_case("KDE") || part.eq_ignore_ascii_case("plasma") {
            return Some(Backend::Kde);
        }
        // "GNOME-Classic", "GNOME-Flashback" and friends are still mutter.
        if part.eq_ignore_ascii_case("GNOME") || part.to_ascii_uppercase().starts_with("GNOME-") {
            return Some(Backend::Gnome);
        }
    }
    None
}

fn detect() -> Backend {
    if let Ok(forced) = std::env::var("QUILL_BACKEND") {
        let picked = match forced.to_ascii_lowercase().as_str() {
            "gnome" | "mutter" => Some(Backend::Gnome),
            "kde" | "kwin" | "plasma" => Some(Backend::Kde),
            _ => None,
        };
        match picked {
            Some(b) => {
                eprintln!("[desktop] QUILL_BACKEND={forced} -- forcing {}", b.name());
                return b;
            }
            None => eprintln!(
                "[desktop] QUILL_BACKEND={forced} is not one of gnome/kde -- ignoring it and detecting"
            ),
        }
    }

    if let Some(b) = std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref().and_then(from_desktop_list) {
        eprintln!("[desktop] detected {} from XDG_CURRENT_DESKTOP", b.name());
        return b;
    }

    // Auto-launched from a udev rule via `SYSTEMD_USER_WANTS`, the unit does
    // not necessarily inherit the graphical session's environment at all --
    // `XDG_CURRENT_DESKTOP` can simply be missing. Asking the session bus who
    // owns mutter's private ScreenCast name answers the same question without
    // depending on any environment variable being imported.
    if dbus_name_has_owner("org.gnome.Mutter.ScreenCast").unwrap_or(false) {
        eprintln!("[desktop] detected {} from the session bus (mutter's ScreenCast name is owned)", Backend::Gnome.name());
        return Backend::Gnome;
    }

    eprintln!(
        "[desktop] no GNOME/KDE marker found (XDG_CURRENT_DESKTOP unset or unrecognized, mutter \
         not on the bus) -- assuming {}. Set QUILL_BACKEND=gnome or =kde to override.",
        Backend::Kde.name()
    );
    Backend::Kde
}

fn dbus_name_has_owner(name: &str) -> Option<bool> {
    block_on_dbus(|| async move {
        let conn = zbus::Connection::session().await.ok()?;
        zbus::fdo::DBusProxy::new(&conn)
            .await
            .ok()?
            .name_has_owner(zbus::names::BusName::try_from(name).ok()?)
            .await
            .ok()
    })
    .flatten()
}

/// Runs one short-lived async D-Bus exchange to completion from a synchronous
/// caller, on a runtime of its own.
///
/// Needed because the two GNOME call sites sit on opposite sides of the
/// process: backend detection runs before `#[tokio::main]`'s runtime is doing
/// anything useful, and `orientation::layout()` is called from the *input*
/// thread (see `input_receiver.rs`'s `Pointer::map`), a plain `std::thread`
/// with no runtime at all. Borrowing the main runtime from either place would
/// mean either blocking inside it or plumbing a handle through code that has
/// no other reason to know about async.
///
/// The thread is scoped, so `make` may borrow from its caller, and the runtime
/// is torn down with the connection when the call returns -- fine for one-shot
/// queries, which is all this is used for. The long-lived ScreenCast session
/// needs a connection that outlives its setup call and uses its own dedicated
/// thread instead; see `gnome_screencast::start`.
pub fn block_on_dbus<T, Fut>(make: impl FnOnce() -> Fut + Send) -> Option<T>
where
    T: Send,
    Fut: std::future::Future<Output = T>,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()?;
                Some(rt.block_on(make()))
            })
            .join()
            .ok()
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_desktop_lists_distributions_actually_ship() {
        assert_eq!(from_desktop_list("KDE"), Some(Backend::Kde));
        assert_eq!(from_desktop_list("ubuntu:GNOME"), Some(Backend::Gnome));
        assert_eq!(from_desktop_list("GNOME-Classic:GNOME"), Some(Backend::Gnome));
        assert_eq!(from_desktop_list("GNOME-Flashback:GNOME"), Some(Backend::Gnome));
        assert_eq!(from_desktop_list("X-Cinnamon"), None);
        assert_eq!(from_desktop_list(""), None);
    }

    /// A GNOME-on-KDE-adjacent list would be ambiguous; first match wins and
    /// that is deliberate, but pin it down so it can't drift silently.
    #[test]
    fn first_recognized_component_wins() {
        assert_eq!(from_desktop_list("KDE:GNOME"), Some(Backend::Kde));
        assert_eq!(from_desktop_list("GNOME:KDE"), Some(Backend::Gnome));
    }
}
