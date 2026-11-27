//! Steam and its game must share the Xwayland server selected by Steam.
//! Redirecting only the game to another display leaves Steam's X11 client broken.
use std::{env, os::unix::net::UnixStream, path::Path};

fn missing_game_display(multiple: bool, display: &str, sockets: &Path) -> bool {
    if !multiple {
        return false;
    }
    let Some(number) = display
        .strip_prefix(':')
        .and_then(|d| d.split('.').next())
        .and_then(|d| d.parse::<u32>().ok())
    else {
        return false;
    };
    let socket = sockets.join(format!("X{number}"));
    // Local X servers may expose an abstract socket instead of a filesystem entry.
    use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};
    if UnixStream::connect(&socket).is_ok() {
        return false;
    }
    SocketAddr::from_abstract_name(socket.as_os_str().as_encoded_bytes())
        .and_then(|addr| UnixStream::connect_addr(&addr))
        .is_err()
}

pub(super) fn display_unavailable() -> bool {
    env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some()
        && missing_game_display(
            env::var("STEAM_MULTIPLE_XWAYLANDS").as_deref() == Ok("1"),
            &env::var("DISPLAY").unwrap_or_default(),
            Path::new("/tmp/.X11-unix"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    #[test]
    fn missing_steam_display_is_detected_even_with_a_healthy_alternate() {
        let dir = tempfile::tempdir().unwrap();
        let _game = UnixListener::bind(dir.path().join("X2")).unwrap();
        assert!(missing_game_display(true, ":1", dir.path()));
        assert!(!missing_game_display(true, ":2.0", dir.path()));
        assert!(!missing_game_display(false, ":1", dir.path()));
        assert!(!missing_game_display(true, "remote:1", dir.path()));
    }
}
