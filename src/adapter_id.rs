//! Shared Bluetooth adapter resolution (no hardcoded `hci0` assumption).
//!
//! Hoisted out of `main.rs` (G7) so that BOTH the daemon (`apply-adapter-config`)
//! and `soundsync doctor` resolve the adapter the same way: `SOUNDSYNC_HCI` if set
//! and non-empty, else the first `/sys/class/bluetooth/hci*` (sorted), else `hci0`
//! as a last resort. This matches the dynamic resolution used in `install.sh:124`
//! and `soundsync-cod-watch.sh:26`.

const SYS_BLUETOOTH_DIR: &str = "/sys/class/bluetooth";
const FALLBACK_ADAPTER: &str = "hci0";

/// Resolve the Bluetooth adapter index from the real environment + sysfs.
///
/// Behaviour is byte-identical to the original private `main.rs::resolve_adapter`:
/// `SOUNDSYNC_HCI` (trimmed, if non-empty) → first sorted `hci*` under
/// `/sys/class/bluetooth` → `hci0`.
pub fn resolve_adapter() -> String {
    let env = std::env::var("SOUNDSYNC_HCI").ok();
    let adapters = list_sys_adapters();
    resolve_adapter_from(env.as_deref(), &adapters)
}

/// Read the `hci*` entries under `/sys/class/bluetooth`, sorted. Empty on any
/// I/O error (matching the original `if let Ok(..)` swallow).
fn list_sys_adapters() -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(SYS_BLUETOOTH_DIR) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("hci"))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// Pure resolver over the (already-read) env value and adapter list, so the
/// override / first-hci / fallback logic is unit-testable without touching the
/// real filesystem or process environment.
///
/// `env` is the raw `SOUNDSYNC_HCI` value (`None` if unset); `adapters` is the
/// already-sorted list of `hci*` names found under `/sys/class/bluetooth`.
fn resolve_adapter_from(env: Option<&str>, adapters: &[String]) -> String {
    if let Some(h) = env {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    if let Some(first) = adapters.first() {
        return first.clone();
    }
    FALLBACK_ADAPTER.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn env_override_wins_over_sysfs() {
        // Non-empty SOUNDSYNC_HCI takes precedence even when adapters exist.
        assert_eq!(
            resolve_adapter_from(Some("hci3"), &v(&["hci0", "hci1"])),
            "hci3"
        );
    }

    #[test]
    fn env_override_is_trimmed() {
        assert_eq!(resolve_adapter_from(Some("  hci2  "), &[]), "hci2");
    }

    #[test]
    fn empty_or_whitespace_env_falls_through_to_first_hci() {
        // Empty / whitespace-only override is ignored; first sorted adapter wins.
        assert_eq!(
            resolve_adapter_from(Some(""), &v(&["hci0", "hci1"])),
            "hci0"
        );
        assert_eq!(
            resolve_adapter_from(Some("   "), &v(&["hci1", "hci0"])),
            "hci1",
            "uses the first element of the (caller-sorted) list as-is"
        );
    }

    #[test]
    fn first_hci_used_when_no_env() {
        assert_eq!(resolve_adapter_from(None, &v(&["hci0", "hci1"])), "hci0");
        // A box whose only adapter is a USB dongle at hci1 (G7's failure case).
        assert_eq!(resolve_adapter_from(None, &v(&["hci1"])), "hci1");
    }

    #[test]
    fn fallback_to_hci0_when_no_env_and_no_adapters() {
        assert_eq!(resolve_adapter_from(None, &[]), "hci0");
        assert_eq!(resolve_adapter_from(Some(""), &[]), "hci0");
    }
}
