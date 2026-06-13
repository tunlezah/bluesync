//! Shared JSON config-persistence primitive for per-user state under
//! `~/.config/soundsync/`. This is the single place that knows how to load and
//! save small JSON state files (e.g. `devices.json`, periodic-interval
//! settings) so the future input-source, null-sink-reconcile, and a2dp-reassert
//! workstreams reuse one robust implementation.
//!
//! Mirrors `bluetooth/name_config.rs` for path resolution (`~/.config/soundsync`,
//! `HOME` with a `/root` fallback) but goes through the `sys::fs::Fs` seam for
//! all I/O so it is unit-testable with a fake filesystem. Loads are total —
//! a missing or corrupt file yields `T::default()` and never panics; saves go
//! through `Fs::write_atomic`, which creates the directory and writes via a
//! temp-file rename.

use crate::sys::fs::Fs;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Base directory for SoundSync per-user config, e.g.
/// `/home/alice/.config/soundsync`. Mirrors `name_config::device_name_path`.
pub fn config_dir(home: &str) -> String {
    format!("{home}/.config/soundsync")
}

/// Resolve the config base directory from `HOME` (falling back to `/root`, as
/// `name_config` and `main.rs` do for the headless appliance/root case).
pub fn config_dir_default() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    config_dir(&home)
}

/// Load `home/.config/soundsync/filename` and deserialize it as `T`.
///
/// Total and panic-free: a missing file, an unreadable file, or invalid JSON
/// all yield `T::default()`. This lets callers treat "no state yet" and
/// "corrupt state" identically — they get sane defaults and can re-persist.
pub fn load_json<T, F>(fs: &F, home: &str, filename: &str) -> T
where
    T: DeserializeOwned + Default,
    F: Fs,
{
    let path = format!("{}/{}", config_dir(home), filename);
    match fs.read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => T::default(),
    }
}

/// Serialize `value` as pretty JSON and persist it to
/// `home/.config/soundsync/filename`.
///
/// Goes through `Fs::write_atomic`, which creates `~/.config/soundsync/` if
/// absent and writes via a temp-file rename so a reader never sees a partial
/// file. Serialization or write failures are returned as `Err(String)`,
/// matching `name_config`'s error contract.
pub fn save_json<T, F>(fs: &F, home: &str, filename: &str, value: &T) -> Result<(), String>
where
    T: Serialize,
    F: Fs,
{
    let contents = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs.write_atomic(&config_dir(home), filename, &contents)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::testing::FakeFs;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Faces {
        devices: Vec<String>,
        interval_secs: u64,
    }

    #[test]
    fn config_dir_format() {
        assert_eq!(config_dir("/home/alice"), "/home/alice/.config/soundsync");
        assert_eq!(config_dir("/root"), "/root/.config/soundsync");
    }

    #[test]
    fn load_returns_default_when_file_absent() {
        let fs = FakeFs::new();
        let got: Faces = load_json(&fs, "/home/alice", "devices.json");
        assert_eq!(got, Faces::default());
    }

    #[test]
    fn load_returns_default_for_corrupt_json() {
        let fs = FakeFs::new().with_file(
            "/home/alice/.config/soundsync/devices.json",
            "{not valid json",
        );
        let got: Faces = load_json(&fs, "/home/alice", "devices.json");
        assert_eq!(got, Faces::default());
    }

    #[test]
    fn save_then_load_round_trip() {
        let fs = FakeFs::new();
        let value = Faces {
            devices: vec!["turntable".to_string(), "cassette".to_string()],
            interval_secs: 30,
        };
        save_json(&fs, "/home/alice", "devices.json", &value).expect("save");
        let got: Faces = load_json(&fs, "/home/alice", "devices.json");
        assert_eq!(got, value);
    }

    #[test]
    fn save_writes_under_config_soundsync_dir() {
        // The directory is created implicitly by `write_atomic`; assert the file
        // lands at the mirrored `~/.config/soundsync/<filename>` path.
        let fs = FakeFs::new();
        let value = Faces {
            interval_secs: 5,
            ..Faces::default()
        };
        save_json(&fs, "/home/alice", "reconcile.json", &value).expect("save");
        assert!(
            fs.written("/home/alice/.config/soundsync/reconcile.json")
                .is_some(),
            "expected file at ~/.config/soundsync/reconcile.json"
        );
    }
}
