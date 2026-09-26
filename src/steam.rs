//! Recognizes windows belonging to a Steam game, so they can be excluded
//! without having to hardcode every game's window class (which varies per
//! game and is often just the game's own binary name).
//!
//! Steam sets a `SteamAppId` (and usually `SteamGameId`) environment
//! variable on every process it launches, native or Proton - this is the
//! same trick tools like GameMode and MangoHud use to recognize a Steam
//! game, and it's far more reliable than matching window classes. As a
//! fallback we also check whether the process executable lives under a
//! `steamapps` (or `~/.steam`) directory.
//!
//! Because some games run behind wrapper processes (Proton, pressure-vessel,
//! reaper) the focused window's immediate PID doesn't always carry the env
//! var itself, so we walk a few steps up the process tree.

use std::fs;

const MAX_ANCESTORS: u8 = 6;

pub fn is_steam_process(pid: i32) -> bool {
    let mut pid = pid;
    for _ in 0..MAX_ANCESTORS {
        if has_steam_env(pid) || has_steam_exe_path(pid) {
            return true;
        }
        match parent_pid(pid) {
            Some(parent) if parent > 1 => pid = parent,
            _ => break,
        }
    }
    false
}

fn has_steam_env(pid: i32) -> bool {
    let Ok(environ) = fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    environ
        .split(|&b| b == 0)
        .any(|kv| kv.starts_with(b"SteamAppId=") || kv.starts_with(b"SteamGameId="))
}

fn has_steam_exe_path(pid: i32) -> bool {
    let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let exe = exe.to_string_lossy().to_lowercase();
    exe.contains("/steamapps/") || exe.contains("/.steam/") || exe.contains("/.local/share/steam/")
}

fn parent_pid(pid: i32) -> Option<i32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Format: "pid (comm) state ppid ...". `comm` can itself contain spaces
    // or parentheses, so split on the *last* ')' rather than whitespace.
    let after_comm = stat.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// Case-insensitive glob match supporting a single trailing `*`, e.g.
/// `"steam_app_*"`. Used for `general.excluded_classes`.
pub fn class_matches(class: &str, pattern: &str) -> bool {
    let class = class.to_lowercase();
    let pattern = pattern.to_lowercase();
    match pattern.strip_suffix('*') {
        Some(prefix) => class.starts_with(prefix),
        None => class == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_prefix_matches() {
        assert!(class_matches("steam_app_1091500", "steam_app_*"));
        assert!(!class_matches("steamwebhelper", "steam_app_*"));
    }

    #[test]
    fn exact_match_is_case_insensitive() {
        assert!(class_matches("Kitty", "kitty"));
        assert!(!class_matches("kittycat", "kitty"));
    }
}
