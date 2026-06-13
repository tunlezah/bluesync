//! Bluetooth device-name persistence. The chosen name is stored in
//! `~/.config/soundsync/device-name` (plain text, trimmed). On any read error
//! the default `"SoundSync"` is returned; write errors are propagated as
//! `Err(String)`.

/// Default device name when no persisted name is found.
pub const DEFAULT_DEVICE_NAME: &str = "SoundSync";

/// Path to the persisted device-name file.
pub fn device_name_path(home: &str) -> String {
    format!("{home}/.config/soundsync/device-name")
}

/// Load the device name from an explicit path, falling back to `DEFAULT_DEVICE_NAME`.
pub fn load_device_name_from(path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let name = s.trim().to_string();
            if name.is_empty() {
                DEFAULT_DEVICE_NAME.to_string()
            } else {
                name
            }
        }
        Err(_) => DEFAULT_DEVICE_NAME.to_string(),
    }
}

/// Load the persisted device name, falling back to `DEFAULT_DEVICE_NAME`.
pub fn load_device_name() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    load_device_name_from(&device_name_path(&home))
}

/// Persist the device name to an explicit path (creates the dir if absent).
pub fn save_device_name_to(path: &str, name: &str) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, name).map_err(|e| e.to_string())
}

/// Persist the device name (creates the config dir if absent).
pub fn save_device_name(name: &str) -> Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    save_device_name_to(&device_name_path(&home), name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_name_path_format() {
        assert_eq!(
            device_name_path("/home/alice"),
            "/home/alice/.config/soundsync/device-name"
        );
        assert_eq!(
            device_name_path("/root"),
            "/root/.config/soundsync/device-name"
        );
    }

    #[test]
    fn load_returns_default_when_file_absent() {
        // No HOME mutation needed — use a path that cannot exist.
        let name = load_device_name_from("/tmp/soundsync-test-nonexistent-zzz/never/device-name");
        assert_eq!(name, DEFAULT_DEVICE_NAME);
    }

    #[test]
    fn load_returns_default_for_empty_file() {
        let path = format!(
            "/tmp/soundsync-test-empty-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        std::fs::write(&path, "   ").unwrap();
        let name = load_device_name_from(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(name, DEFAULT_DEVICE_NAME);
    }

    #[test]
    fn save_and_load_round_trip() {
        // Avoid set_var (not thread-safe with parallel tests): exercise the
        // explicit-path helpers instead.
        let dir = format!(
            "/tmp/soundsync-test-rl-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let path = format!("{dir}/.config/soundsync/device-name");
        save_device_name_to(&path, "TestDevice").expect("save");
        let loaded = load_device_name_from(&path);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(loaded, "TestDevice");
    }
}
