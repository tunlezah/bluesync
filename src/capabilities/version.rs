//! WirePlumber version detection + config-format decision (AUD-017/018).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// WirePlumber 0.4.x — Lua under bluetooth.lua.d
    Lua,
    /// WirePlumber 0.5+ — SPA-JSON under wireplumber.conf.d
    SpaJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WpVersion {
    pub major: u32,
    pub minor: u32,
}

impl WpVersion {
    /// Parse the first `MAJOR.MINOR` token from `wireplumber --version` output.
    pub fn parse(output: &str) -> Option<WpVersion> {
        for tok in output.split(|c: char| c.is_whitespace()) {
            let mut it = tok.split('.');
            if let (Some(a), Some(b)) = (it.next(), it.next()) {
                if let (Ok(major), Ok(minor)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    return Some(WpVersion { major, minor });
                }
            }
        }
        None
    }

    /// >= 0.5 uses SPA-JSON; older uses Lua.
    pub fn config_format(self) -> ConfigFormat {
        if (self.major, self.minor) >= (0, 5) {
            ConfigFormat::SpaJson
        } else {
            ConfigFormat::Lua
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_known_outputs() {
        assert_eq!(
            WpVersion::parse("wireplumber 0.4.17"),
            Some(WpVersion { major: 0, minor: 4 })
        );
        assert_eq!(
            WpVersion::parse("0.5.2"),
            Some(WpVersion { major: 0, minor: 5 })
        );
        assert_eq!(
            WpVersion::parse("WirePlumber\n0.5.0\n..."),
            Some(WpVersion { major: 0, minor: 5 })
        );
    }
    #[test]
    fn unparseable_is_none() {
        assert_eq!(WpVersion::parse("garbage"), None);
    }
    #[test]
    fn config_format_decision() {
        assert_eq!(
            WpVersion { major: 0, minor: 4 }.config_format(),
            ConfigFormat::Lua
        );
        assert_eq!(
            WpVersion { major: 0, minor: 5 }.config_format(),
            ConfigFormat::SpaJson
        );
        assert_eq!(
            WpVersion { major: 1, minor: 0 }.config_format(),
            ConfigFormat::SpaJson
        );
    }
}
