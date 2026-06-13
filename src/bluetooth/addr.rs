//! BlueZ MAC <-> D-Bus object-path conversion (AUD-012).
//! Handles transport paths (trailing /sepN/fdN) by finding the `dev_` segment.

/// `/org/bluez/hci0/dev_AA_BB_..` -> `AA:BB:..` (also works on transport paths).
pub fn address_from_path(path: &str) -> Option<String> {
    path.split('/')
        .find(|seg| seg.starts_with("dev_"))
        .and_then(|seg| seg.strip_prefix("dev_"))
        .map(|s| s.replace('_', ":"))
}

/// `("/org/bluez/hci0", "AA:BB:..")` -> `/org/bluez/hci0/dev_AA_BB_..`.
pub fn path_from_address(adapter_path: &str, address: &str) -> String {
    format!("{}/dev_{}", adapter_path, address.replace(':', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_from_device_path() {
        assert_eq!(
            address_from_path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
    }
    #[test]
    fn address_from_transport_path() {
        assert_eq!(
            address_from_path("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/sep1/fd0").as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
    }
    #[test]
    fn invalid_paths_return_none() {
        assert_eq!(address_from_path("/org/bluez/hci0"), None);
        assert_eq!(address_from_path("/org/bluez/hci0/player0"), None);
    }
    #[test]
    fn path_roundtrip() {
        let addr = "12:34:56:78:9A:BC";
        let path = path_from_address("/org/bluez/hci0", addr);
        assert_eq!(path, "/org/bluez/hci0/dev_12_34_56_78_9A_BC");
        assert_eq!(address_from_path(&path).as_deref(), Some(addr));
    }
}
