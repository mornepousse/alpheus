//! Which compositor the session runs, from its environment variables: shared by the daemon (which
//! opens its IPC read-only) and the TUI (which never writes to it). Nothing here opens a socket.

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compositor {
    Sway { socket: PathBuf },
    Hyprland { directory: PathBuf },
    Mango { socket: PathBuf },
    Niri { socket: PathBuf },
}

impl Compositor {
    /// The path of the compositor's IPC socket: the socket itself, or for Hyprland the
    /// `.socket.sock` of its instance directory. Only named, never opened here.
    pub fn ipc_socket(&self) -> PathBuf {
        match self {
            Compositor::Sway { socket } | Compositor::Mango { socket } | Compositor::Niri { socket } => socket.clone(),
            Compositor::Hyprland { directory } => directory.join(".socket.sock"),
        }
    }
}

/// The four variables, in detection order (Mango and Sway spec § 1).
pub const VARIABLES: [&str; 4] = ["SWAYSOCK", "HYPRLAND_INSTANCE_SIGNATURE", "MANGO_INSTANCE_SIGNATURE", "NIRI_SOCKET"];

/// The usual session-script order, with niri last: SWAYSOCK,
/// then HYPRLAND_INSTANCE_SIGNATURE, then MANGO_INSTANCE_SIGNATURE, then NIRI_SOCKET. An empty
/// variable counts as unset; the first one set decides, and an unusable value gives no compositor
/// rather than the next variable's (a nested session inherits several of them).
pub fn detect(var: impl Fn(&str) -> Option<OsString>) -> Option<Compositor> {
    let non_empty = |k: &str| var(k).filter(|v| !v.is_empty());
    if let Some(socket) = non_empty("SWAYSOCK") {
        return Some(Compositor::Sway { socket: socket.into() });
    }
    if let Some(sig) = non_empty("HYPRLAND_INSTANCE_SIGNATURE") {
        // The signature is a directory name: "/" or ".." would escape it.
        if sig == ".." || sig.as_encoded_bytes().contains(&b'/') {
            return None;
        }
        let runtime = non_empty("XDG_RUNTIME_DIR")?;
        return Some(Compositor::Hyprland { directory: PathBuf::from(runtime).join("hypr").join(sig) });
    }
    if non_empty("MANGO_INSTANCE_SIGNATURE").is_some() {
        return mango_socket(&var).map(|socket| Compositor::Mango { socket });
    }
    non_empty("NIRI_SOCKET").map(|socket| Compositor::Niri { socket: socket.into() })
}

/// `$MANGO_INSTANCE_SIGNATURE` is the socket's own path (`/run/user/1000/mango-<pid>.sock`),
/// absolute only. Read at detection, and again at each session start (spec § 2: the path embeds
/// the compositor's pid).
pub fn mango_socket(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    var("MANGO_INSTANCE_SIGNATURE").filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositor_ipc_sockets() {
        let sock = |p: &str| PathBuf::from(p);
        assert_eq!(Compositor::Sway { socket: sock("/run/user/1000/sway.sock") }.ipc_socket(), sock("/run/user/1000/sway.sock"));
        assert_eq!(Compositor::Mango { socket: sock("/run/user/1000/mango-1.sock") }.ipc_socket(), sock("/run/user/1000/mango-1.sock"));
        assert_eq!(Compositor::Niri { socket: sock("/run/user/1000/niri.sock") }.ipc_socket(), sock("/run/user/1000/niri.sock"));
        assert_eq!(
            Compositor::Hyprland { directory: sock("/run/user/1000/hypr/abc") }.ipc_socket(),
            sock("/run/user/1000/hypr/abc/.socket.sock")
        );
    }
}
