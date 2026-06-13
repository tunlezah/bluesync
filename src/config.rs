//! Runtime configuration resolvers. All values are driven by environment
//! variables, with appliance-appropriate defaults. The resolvers are pure
//! functions (take the env value already extracted) so they're trivially
//! unit-tested without mutating the process environment.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Default bind address: all interfaces, port 8080. A headless appliance is
/// controlled from other devices on the same LAN; loopback would prevent that.
pub const DEFAULT_BIND: &str = "0.0.0.0:8080";

/// Default web-UI directory: the SPA installed by the package.
pub const DEFAULT_WEB_DIR: &str = "/usr/share/soundsync/webui";

/// Default Bluetooth advertised name (used when no persisted name is found and
/// `SOUNDSYNC_NAME` is not set).
pub const DEFAULT_NAME: &str = "SoundSync";

/// Resolve the bind address from `SOUNDSYNC_BIND` (already read by the caller)
/// or the built-in default. Returns a clear error string on a parse failure so
/// the caller can `eprintln!` and exit 1.
pub fn resolve_bind(env_val: Option<&str>) -> Result<SocketAddr, String> {
    let s = env_val.unwrap_or(DEFAULT_BIND);
    s.parse::<SocketAddr>()
        .map_err(|e| format!("SOUNDSYNC_BIND={s:?} is not a valid socket address: {e}"))
}

/// Resolve the web-UI directory from `SOUNDSYNC_WEB_DIR` (already read by the
/// caller) or the built-in default.
pub fn resolve_web_dir(env_val: Option<&str>) -> PathBuf {
    PathBuf::from(env_val.unwrap_or(DEFAULT_WEB_DIR))
}

/// Resolve the device name. Precedence (highest first):
/// 1. `persisted` — the user-chosen name stored in `~/.config/soundsync/device-name`;
/// 2. `env_val` — `SOUNDSYNC_NAME` environment variable;
/// 3. `DEFAULT_NAME`.
pub fn resolve_name(env_val: Option<&str>, persisted: Option<String>) -> String {
    if let Some(p) = persisted.filter(|s| !s.is_empty()) {
        return p;
    }
    if let Some(e) = env_val.filter(|s| !s.is_empty()) {
        return e.to_string();
    }
    DEFAULT_NAME.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_bind ──────────────────────────────────────────────────────────

    #[test]
    fn default_bind_parses() {
        let addr = resolve_bind(None).expect("default bind should parse");
        assert_eq!(addr.port(), 8080);
        assert_eq!(addr.ip().to_string(), "0.0.0.0");
    }

    #[test]
    fn custom_bind_parses() {
        let addr = resolve_bind(Some("127.0.0.1:9090")).expect("custom bind should parse");
        assert_eq!(addr.port(), 9090);
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
    }

    #[test]
    fn bad_bind_returns_err_with_value() {
        let err = resolve_bind(Some("not-an-addr")).unwrap_err();
        assert!(
            err.contains("not-an-addr"),
            "error should include the bad value, got: {err}"
        );
        assert!(
            err.contains("SOUNDSYNC_BIND"),
            "error should mention env var, got: {err}"
        );
    }

    // ── resolve_web_dir ───────────────────────────────────────────────────────

    #[test]
    fn web_dir_default() {
        assert_eq!(resolve_web_dir(None), PathBuf::from(DEFAULT_WEB_DIR));
    }

    #[test]
    fn web_dir_override() {
        assert_eq!(
            resolve_web_dir(Some("/opt/soundsync/webui")),
            PathBuf::from("/opt/soundsync/webui")
        );
    }

    // ── resolve_name ──────────────────────────────────────────────────────────

    #[test]
    fn name_persisted_wins_over_env_and_default() {
        let name = resolve_name(Some("EnvName"), Some("PersistedName".to_string()));
        assert_eq!(name, "PersistedName");
    }

    #[test]
    fn name_env_wins_when_no_persisted() {
        let name = resolve_name(Some("EnvName"), None);
        assert_eq!(name, "EnvName");
    }

    #[test]
    fn name_env_wins_when_persisted_is_empty() {
        let name = resolve_name(Some("EnvName"), Some(String::new()));
        assert_eq!(name, "EnvName");
    }

    #[test]
    fn name_default_when_all_absent() {
        let name = resolve_name(None, None);
        assert_eq!(name, DEFAULT_NAME);
    }

    #[test]
    fn name_default_when_env_empty_and_no_persisted() {
        let name = resolve_name(Some(""), None);
        assert_eq!(name, DEFAULT_NAME);
    }
}
