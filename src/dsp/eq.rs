//! 10-band parametric EQ -> PipeWire filter-chain config generation (AUD-025).
use serde::{Deserialize, Serialize};

pub const NUM_BANDS: usize = 10;
pub const MIN_GAIN_DB: f32 = -12.0;
pub const MAX_GAIN_DB: f32 = 12.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FilterType {
    LowShelf,
    Peaking,
    HighShelf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqBand {
    pub freq: f64,
    /// f32 is sufficient for a dB gain and matches PipeWire's `"Gain"` control
    /// type and the web UI slider; `freq`/`q` stay f64 for DSP precision.
    pub gain_db: f32,
    pub q: f64,
    pub filter_type: FilterType,
}

impl EqBand {
    /// Clamp gain to [-12.0, 12.0] dB.
    pub fn clamp_gain(&mut self) {
        self.gain_db = self.gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    }
}

/// Default 10-band EQ (all gains 0 dB).
pub fn default_bands() -> [EqBand; NUM_BANDS] {
    [
        EqBand {
            freq: 60.0,
            gain_db: 0.0,
            q: 0.707,
            filter_type: FilterType::LowShelf,
        },
        EqBand {
            freq: 120.0,
            gain_db: 0.0,
            q: 1.414,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 250.0,
            gain_db: 0.0,
            q: 1.414,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 500.0,
            gain_db: 0.0,
            q: 1.414,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 1000.0,
            gain_db: 0.0,
            q: 1.414,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 2000.0,
            gain_db: 0.0,
            q: 1.414,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 4000.0,
            gain_db: 0.0,
            q: 1.414,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 8000.0,
            gain_db: 0.0,
            q: 1.820,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 12000.0,
            gain_db: 0.0,
            q: 2.870,
            filter_type: FilterType::Peaking,
        },
        EqBand {
            freq: 16000.0,
            gain_db: 0.0,
            q: 0.707,
            filter_type: FilterType::HighShelf,
        },
    ]
}

fn filter_label(filter_type: FilterType) -> &'static str {
    match filter_type {
        FilterType::LowShelf => "bq_lowshelf",
        FilterType::Peaking => "bq_peaking",
        FilterType::HighShelf => "bq_highshelf",
    }
}

/// Generate the PipeWire filter-chain config string from EQ bands.
///
/// # Panics
/// Panics if `bands` is empty (an empty filter chain would reference
/// non-existent `eq_band_0` ports and produce an invalid config).
pub fn generate_filter_chain_config(bands: &[EqBand]) -> String {
    assert!(
        !bands.is_empty(),
        "generate_filter_chain_config requires at least one band"
    );
    let mut nodes = String::new();
    let mut links = String::new();

    for (i, band) in bands.iter().enumerate() {
        let label = filter_label(band.filter_type);
        nodes.push_str(&format!(
            "                    {{ type = builtin  label = {}  name = eq_band_{}\n\
             \x20                     control = {{ \"Freq\" = {}  \"Q\" = {:.3}  \"Gain\" = {:.1} }} }}\n",
            label, i, band.freq, band.q, band.gain_db
        ));
        if i > 0 {
            links.push_str(&format!(
                "                    {{ output = \"eq_band_{}:Out\"  input = \"eq_band_{}:In\" }}\n",
                i - 1, i
            ));
        }
    }

    let first_band = "eq_band_0";
    let last_band = format!("eq_band_{}", bands.len().saturating_sub(1));

    format!(
        r#"# PipeWire filter-chain for 10-band parametric EQ (auto-generated)
context.modules = [
    {{ name = libpipewire-module-filter-chain
        args = {{
            node.name = "soundsync-eq"
            node.description = "SoundSync Equalizer"
            media.name = "SoundSync EQ"
            capture.props = {{
                node.name = "effect_input.soundsync-eq"
                media.class = "Audio/Sink"
                audio.rate = 48000
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
            playback.props = {{
                node.name = "effect_output.soundsync-eq"
                media.class = "Stream/Output/Audio"
                node.target = "soundsync-capture"
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
            filter.graph = {{
                nodes = [
{nodes}                ]
                links = [
{links}                ]
                inputs  = [ "{first_band}:In" ]
                outputs = [ "{last_band}:Out" ]
            }}
        }}
    }}
]
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bands_shape() {
        let b = default_bands();
        assert_eq!(b.len(), NUM_BANDS);
        assert_eq!(b[0].freq, 60.0);
        assert_eq!(b[4].freq, 1000.0);
        assert_eq!(b[9].freq, 16000.0);
        assert_eq!(b[0].filter_type, FilterType::LowShelf);
        assert_eq!(b[9].filter_type, FilterType::HighShelf);
        assert!(b[1..9].iter().all(|x| x.filter_type == FilterType::Peaking));
        assert!(b.iter().all(|x| x.gain_db == 0.0));
        // Load-bearing Q calibration values (a typo here would skew the EQ silently).
        assert_eq!(b[0].q, 0.707);
        assert_eq!(b[1].q, 1.414);
        assert_eq!(b[7].q, 1.820);
        assert_eq!(b[8].q, 2.870);
        assert_eq!(b[9].q, 0.707);
    }

    #[test]
    fn config_contains_load_bearing_fields() {
        let cfg = generate_filter_chain_config(&default_bands());
        for needle in [
            "effect_input.soundsync-eq",
            "effect_output.soundsync-eq",
            "node.target = \"soundsync-capture\"",
            "audio.rate = 48000",
            "audio.position = [ FL FR ]",
            "\"Freq\" =",
            "\"Q\" =",
            "inputs  = [ \"eq_band_0:In\" ]",
            "outputs = [ \"eq_band_9:Out\" ]",
        ] {
            assert!(cfg.contains(needle), "missing: {needle}");
        }
    }

    #[test]
    fn gains_are_rendered() {
        let mut b = default_bands().to_vec();
        b[0].gain_db = 6.0;
        b[9].gain_db = -3.0;
        let cfg = generate_filter_chain_config(&b);
        assert!(cfg.contains("\"Gain\" = 6.0"));
        assert!(cfg.contains("\"Gain\" = -3.0"));
    }

    #[test]
    fn clamp_gain_bounds() {
        let mut band = EqBand {
            freq: 1000.0,
            gain_db: 20.0,
            q: 1.0,
            filter_type: FilterType::Peaking,
        };
        band.clamp_gain();
        assert_eq!(band.gain_db, 12.0);
        band.gain_db = -20.0;
        band.clamp_gain();
        assert_eq!(band.gain_db, -12.0);
    }
}
