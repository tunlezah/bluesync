//! Version-aware WirePlumber BlueZ config generation (AUD-004/013/017/018).
use crate::capabilities::version::ConfigFormat;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WpConfigFile {
    pub filename: String,
    pub etc_dir: &'static str,
    pub contents: String,
}

const SPA_JSON: &str = r#"# SoundSync — WirePlumber 0.5+ A2DP sink config.
# CRITICAL: monitor.bluez.seat-monitoring = disabled — without it, headless/
# SSH-only sessions never start the BlueZ monitor, so a connected phone pairs
# but exposes no audio sink. (AUD-004)
wireplumber.profiles = {
    main = {
        monitor.bluez.seat-monitoring = disabled
    }
}

monitor.bluez.properties = {
    bluez5.roles = [ a2dp_sink a2dp_source hfp_hf hfp_ag hsp_hs hsp_ag ]
    bluez5.codecs = [ sbc aac ldac aptx aptx_hd ]
    bluez5.enable-sbc-xq = true
    bluez5.enable-msbc = false
    bluez5.enable-hw-volume = true
}
"#;

const LUA: &str = r#"-- SoundSync: Enable A2DP sink role so Bluetooth devices can stream audio here.
-- IMPORTANT: modify INDIVIDUAL properties — do NOT replace the entire
-- bluez_monitor.properties table, as that wipes defaults like with-logind. (AUD-018)
bluez_monitor.properties["bluez5.roles"] = "[ a2dp_sink ]"
bluez_monitor.properties["bluez5.codecs"] = "[ sbc aac ldac aptx aptx_hd ]"
bluez_monitor.properties["bluez5.enable-sbc-xq"] = true
bluez_monitor.properties["bluez5.enable-msbc"] = false
bluez_monitor.properties["bluez5.enable-hw-volume"] = true
"#;

pub fn generate(format: ConfigFormat) -> WpConfigFile {
    match format {
        ConfigFormat::SpaJson => WpConfigFile {
            filename: "51-soundsync.conf".to_string(),
            etc_dir: "/etc/wireplumber/wireplumber.conf.d",
            contents: SPA_JSON.to_string(),
        },
        ConfigFormat::Lua => WpConfigFile {
            filename: "51-soundsync-a2dp.lua".to_string(),
            etc_dir: "/etc/wireplumber/bluetooth.lua.d",
            contents: LUA.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::version::ConfigFormat;

    #[test]
    fn spa_json_has_seat_monitoring_and_roles() {
        let c = generate(ConfigFormat::SpaJson);
        assert_eq!(c.filename, "51-soundsync.conf");
        assert!(c
            .contents
            .contains("monitor.bluez.seat-monitoring = disabled"));
        assert!(c
            .contents
            .contains("bluez5.roles = [ a2dp_sink a2dp_source hfp_hf hfp_ag hsp_hs hsp_ag ]"));
        assert!(c
            .contents
            .contains("bluez5.codecs = [ sbc aac ldac aptx aptx_hd ]"));
        assert!(c.contents.contains("bluez5.enable-sbc-xq = true"));
        assert!(c.contents.contains("bluez5.enable-msbc = false"));
    }

    #[test]
    fn lua_sets_individual_props_only_a2dp_sink() {
        let c = generate(ConfigFormat::Lua);
        assert_eq!(c.filename, "51-soundsync-a2dp.lua");
        assert!(c
            .contents
            .contains("bluez_monitor.properties[\"bluez5.roles\"] = \"[ a2dp_sink ]\""));
        assert!(c.contents.contains(
            "bluez_monitor.properties[\"bluez5.codecs\"] = \"[ sbc aac ldac aptx aptx_hd ]\""
        ));
        assert!(!c.contents.contains("bluez_monitor.properties = {"));
    }

    #[test]
    fn install_dir_matches_format() {
        assert_eq!(
            generate(ConfigFormat::SpaJson).etc_dir,
            "/etc/wireplumber/wireplumber.conf.d"
        );
        assert_eq!(
            generate(ConfigFormat::Lua).etc_dir,
            "/etc/wireplumber/bluetooth.lua.d"
        );
    }
}
