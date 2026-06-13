//! Discovery parsers for avahi-browse (Chromecast) and pactl (sound cards + AirPlay).
//!
//! All parsers are pure `&str → Vec<OutputDevice>` — no I/O, fully unit-tested.
//! The async `probe_outputs` wraps them with `CommandRunner` calls (best-effort:
//! a failed command yields an empty list for that kind, never an error).
//!
//! Source of truth per device kind:
//! - **AirPlay** — `parse_raop_sinks` applied to `pactl list short sinks` output;
//!   requires `module-raop-discover` to be loaded so PipeWire exposes RAOP sinks.
//! - **Chromecast** — `parse_chromecasts` applied to
//!   `avahi-browse -rtp _googlecast._tcp` output.
//! - **Soundcard** — `parse_soundcard_sinks` applied to `pactl list short sinks`.

use std::collections::HashSet;
use std::hash::Hash;

use crate::output::{AvailableOutputs, OutputDevice, OutputKind};
use crate::sys::command::CommandRunner;

// ─── Dedup helper ────────────────────────────────────────────────────────────

/// Order-preserving, first-wins deduplication.
///
/// Iterates `devices` once, keeping the first entry for each unique key
/// produced by `key_fn`. Duplicate entries (same key, later position) are
/// dropped. The relative order of kept entries is unchanged.
///
/// Used to collapse avahi's repeated IPv4/IPv6/re-announcement lines for the
/// same Chromecast (dedup by `id`) and PipeWire's many near-identical RAOP
/// bridge sinks (dedup by friendly `name`).
pub fn dedup_by<K, F>(devices: Vec<OutputDevice>, key_fn: F) -> Vec<OutputDevice>
where
    K: Eq + Hash,
    F: Fn(&OutputDevice) -> K,
{
    let mut seen = HashSet::new();
    devices
        .into_iter()
        .filter(|d| seen.insert(key_fn(d)))
        .collect()
}

// ─── avahi TXT helpers ───────────────────────────────────────────────────────

/// Extract the value of a `key=value` pair from a space-separated list of
/// double-quoted TXT tokens like `"fn=Guest TV" "id=d122a7025b65"`.
///
/// The tokens are delimited by `"..."` pairs. Values may contain spaces
/// (e.g. `fn=Guest TV`), so we split by `"` and look for `key=` inside each
/// token, rather than splitting on whitespace.
fn extract_txt_value<'a>(txt_tokens: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    // Split on `"` — odd-index substrings are the contents inside quotes.
    for (i, chunk) in txt_tokens.split('"').enumerate() {
        if i % 2 == 1 {
            // Inside a quoted token.
            if let Some(val) = chunk.strip_prefix(prefix.as_str()) {
                return Some(val);
            }
        }
    }
    None
}

// ─── Chromecast ──────────────────────────────────────────────────────────────

/// Parse `avahi-browse -rtp _googlecast._tcp` output into Chromecast devices.
///
/// Only `=` (resolved) lines are processed; all other lines are skipped.
///
/// avahi-browse parseable format (`;`-separated, 0-indexed):
/// `=;IFACE;PROTO;SERVICE_NAME;TYPE;DOMAIN;HOSTNAME;ADDRESS;PORT;TXT…`
///
/// Field indices:
/// - 2: protocol (`IPv4` or `IPv6`)
/// - 7: routable IP address (used as `addr` for TLS connect)
/// - 8: port
/// - 9: TXT (space-separated double-quoted key=value tokens)
///
/// Extracted values:
/// - name: `fn=` TXT token (may contain spaces, e.g. `fn=Guest TV`)
/// - id:   `id=` TXT token
/// - addr: fields[7] (routable IP address — IPv4 preferred over IPv6)
/// - port: fields[8]
///
/// **IPv4 preference:** avahi emits both an `IPv4` and an `IPv6` line for the
/// same device.  This function returns IPv4 lines before IPv6 lines (stable
/// sort by protocol, IPv4 first) so that the caller's `dedup_by(…, |d| d.id)`
/// naturally retains the IPv4 address for the device.
pub fn parse_chromecasts(avahi: &str) -> Vec<OutputDevice> {
    // Each entry carries a flag indicating whether it came from an IPv4 line
    // so we can sort IPv4 first before returning.
    let mut entries: Vec<(OutputDevice, bool)> = Vec::new();

    for line in avahi.lines() {
        let line = line.trim();
        if !line.starts_with('=') {
            continue;
        }
        let fields: Vec<&str> = line.splitn(10, ';').collect();
        if fields.len() < 10 {
            continue;
        }
        // fields[2] = "IPv4" or "IPv6"
        let is_ipv4 = fields[2] == "IPv4";
        // fields[7] = routable IP address (IPv4 a.b.c.d or IPv6 fe80::…)
        let addr = fields[7].to_string();
        let port: u16 = match fields[8].parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let txt = fields[9];
        let name = match extract_txt_value(txt, "fn") {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let id = match extract_txt_value(txt, "id") {
            Some(i) if !i.is_empty() => i.to_string(),
            _ => continue,
        };
        entries.push((
            OutputDevice {
                kind: OutputKind::Chromecast,
                id,
                name,
                addr: Some(addr),
                port: Some(port),
            },
            is_ipv4,
        ));
    }

    // Stable sort: IPv4 entries first, IPv6 entries after.  This ensures that
    // when the caller deduplicates by `id` with first-wins semantics, the IPv4
    // address is always preferred over the IPv6 address.
    entries.sort_by_key(|(_, is_ipv4)| if *is_ipv4 { 0u8 } else { 1u8 });

    entries.into_iter().map(|(d, _)| d).collect()
}

// ─── AirPlay: RAOP PipeWire sinks ────────────────────────────────────────────

/// Parse `pactl list short sinks` output into AirPlay devices backed by
/// PipeWire RAOP sinks.
///
/// When `module-raop-discover` is loaded, PipeWire exposes one sink per
/// discovered AirPlay receiver with the name pattern:
/// `raop_sink.<FRIENDLY>.local.<IP>.<PORT>`
///
/// This function filters to lines whose sink name starts with `raop_sink.`,
/// then extracts:
/// - `id`   = the full sink name (used to route via `module-loopback`)
/// - `name` = the friendly segment between `raop_sink.` and `.local`
///   (falls back to the whole remainder if `.local` is absent)
///
/// `addr` and `port` are left as `None`; they can be recovered from the sink
/// name itself but are not needed for routing.
///
/// `pactl list short sinks` format (tab-separated):
/// `<idx>\t<name>\t<driver>\t<spec>\t<state>`
pub fn parse_raop_sinks(pactl_sinks: &str) -> Vec<OutputDevice> {
    let mut devices = Vec::new();
    for line in pactl_sinks.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        if fields.len() < 2 {
            continue;
        }
        let sink_name = fields[1];
        if !sink_name.starts_with("raop_sink.") {
            continue;
        }
        // Strip the "raop_sink." prefix to get "<FRIENDLY>.local.<IP>.<PORT>".
        let remainder = &sink_name["raop_sink.".len()..];
        // Friendly name = up to (but not including) the LAST ".local".
        // Using rfind ensures correctness for names that themselves contain ".local".
        let friendly = if let Some(pos) = remainder.rfind(".local") {
            &remainder[..pos]
        } else {
            remainder
        };
        devices.push(OutputDevice {
            kind: OutputKind::Airplay,
            id: sink_name.to_string(),
            name: friendly.to_string(),
            addr: None,
            port: None,
        });
    }
    devices
}

// ─── Sound-card sinks ────────────────────────────────────────────────────────

/// Parse `pactl list short sinks` output into hardware sound-card sinks.
///
/// Format (tab-separated): `<idx>\t<name>\t<driver>\t<spec>\t<state>`
///
/// Hardware sinks are those whose `name`:
/// - does NOT start with `soundsync` (our own capture null-sink / effects),
/// - does NOT start with `raop_sink.` (PipeWire RAOP/AirPlay sinks — OUT-3),
/// - does NOT contain `effect_` (effect sinks),
/// - does NOT end with `.monitor` (monitor sources misidentified as sinks).
pub fn parse_soundcard_sinks(pactl: &str) -> Vec<OutputDevice> {
    let mut devices = Vec::new();
    for line in pactl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        if fields.len() < 2 {
            continue;
        }
        let name = fields[1];
        // Filter out our own sinks, RAOP sinks, and monitors.
        if name.starts_with("soundsync")
            || name.starts_with("raop_sink.")
            || name.contains("effect_")
            || name.ends_with(".monitor")
        {
            continue;
        }
        devices.push(OutputDevice {
            kind: OutputKind::Soundcard,
            id: name.to_string(),
            name: name.to_string(),
            addr: Option::None,
            port: Option::None,
        });
    }
    devices
}

// ─── Module list parser ──────────────────────────────────────────────────────

/// Parse `pactl list short modules` output and return the numeric id of the
/// first row whose module name equals `module_name`.
///
/// `pactl list short modules` format (tab-separated):
/// `<id>\t<module-name>\t<args>`
///
/// Returns `None` if `module_name` is not present or the id cannot be parsed
/// as `u32`.  Garbage / short lines are silently skipped — the function is
/// safe to call with any `&str`.
pub fn parse_module_id_by_name(pactl_modules: &str, module_name: &str) -> Option<u32> {
    for line in pactl_modules.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.splitn(3, '\t');
        let id_str = match fields.next() {
            Some(s) => s,
            None => continue,
        };
        let name = match fields.next() {
            Some(s) => s,
            None => continue,
        };
        if name == module_name {
            return id_str.parse::<u32>().ok();
        }
    }
    None
}

// ─── Sync probe (glue) ───────────────────────────────────────────────────────

/// Run all discovery commands and aggregate results.
///
/// Best-effort: a failed command (binary missing, permission error, etc.) yields
/// an empty list for that kind — never propagates the error.
///
/// AirPlay devices come from `parse_raop_sinks` applied to `pactl list short
/// sinks` (requires `module-raop-discover` to be loaded so the RAOP sinks
/// exist).  The same `pactl` output is reused for soundcard discovery.
///
/// Chromecast devices come from `avahi-browse -rtp _googlecast._tcp`.
///
/// This function is synchronous because `CommandRunner::run` is synchronous.
/// In the async controller, call via `tokio::task::spawn_blocking` or accept
/// that brief synchronous calls are negligible for these fast local commands.
pub fn probe_outputs<R: CommandRunner>(runner: &R) -> AvailableOutputs {
    // Dedup Chromecasts by the stable `id=` TXT field — avahi returns each device
    // multiple times (IPv4 + IPv6 + re-announcements + cast-group entries).
    let chromecast = dedup_by(
        runner
            .run("avahi-browse", &["-rtp", "_googlecast._tcp"])
            .map(|o| parse_chromecasts(&o.stdout))
            .unwrap_or_default(),
        |d| d.id.clone(),
    );

    // One `pactl list short sinks` call feeds both soundcard and airplay parsers.
    // Dedup soundcard by id (stable PW sink name — cheap safety).
    // Dedup airplay by friendly name — collapses many near-identical RAOP bridge
    // sinks (e.g. multiple `a0d7b954-aircast` entries) while keeping real devices.
    let (soundcard, airplay) = runner
        .run("pactl", &["list", "short", "sinks"])
        .map(|o| {
            (
                dedup_by(parse_soundcard_sinks(&o.stdout), |d| d.id.clone()),
                dedup_by(parse_raop_sinks(&o.stdout), |d| d.name.clone()),
            )
        })
        .unwrap_or_default();

    AvailableOutputs {
        soundcard,
        airplay,
        chromecast,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::FakeCommandRunner;

    // Real sample line from the plan (reformatted to fit in source).
    // This is an IPv6 line — addr will be the actual IP (fe80::1), not the hostname.
    const CHROMECAST_SAMPLE: &str = concat!(
        "=;wlp2s0;IPv6;Chromecast-HD-d122;_googlecast._tcp;local;",
        "d122.local;fe80::1;8009;",
        "\"rs=\" \"fn=Guest TV\" \"md=Chromecast HD\" \"id=d122a7025b65\" \"ve=05\""
    );

    const PACTL_SAMPLE: &str = "\
0\talsa_output.usb-FiiO_BTR3-00.analog-stereo\tmodule-alsa-card\ts32le 2ch 44100Hz\tSUSPENDED
1\talsa_output.pci-0000_00_1f.3.analog-stereo\tmodule-alsa-card\ts16le 2ch 48000Hz\tSUSPENDED
2\tsoundsync-capture\tmodule-null-sink\ts16le 2ch 48000Hz\tIDLE
3\tsoundsync-effect_eq\tmodule-null-sink\ts16le 2ch 48000Hz\tIDLE
4\tsoundsync-capture.monitor\tmodule-null-sink\ts16le 2ch 48000Hz\tIDLE
";

    /// Real host samples from the plan (verified on the actual device).
    const RAOP_PACTL_SAMPLE: &str = "\
0\talsa_output.usb-FiiO_BTR3-00.analog-stereo\tmodule-alsa-card\ts32le 2ch 44100Hz\tSUSPENDED
1\tsoundsync-capture\tmodule-null-sink\ts16le 2ch 48000Hz\tIDLE
2\traop_sink.HP-L.local.192.168.1.107.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
3\traop_sink.Lounge.local.192.168.1.232.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
4\traop_sink.Gym.local.192.168.1.115.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
5\traop_sink.Bose-cba4425d-abcd.local.192.168.1.150.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
";

    // ── Chromecast ────────────────────────────────────────────────────────────

    #[test]
    fn parse_chromecasts_extracts_fn_and_id() {
        let devices = parse_chromecasts(CHROMECAST_SAMPLE);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.kind, OutputKind::Chromecast);
        assert_eq!(d.name, "Guest TV");
        assert_eq!(d.id, "d122a7025b65");
        // addr is the routable IP address (field[7]), not the hostname.
        assert_eq!(d.addr.as_deref(), Some("fe80::1"));
        assert_eq!(d.port, Some(8009));
    }

    #[test]
    fn parse_chromecasts_skips_non_equal_lines() {
        let input = "+;eth0;IPv4;SomeCast;_googlecast._tcp;local\n\
                     =;eth0;IPv4;Cast2;_googlecast._tcp;local;cast2.local;10.0.0.1;8009;\
                     \"fn=Living Room\" \"id=aabbcc\"";
        let devices = parse_chromecasts(input);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Living Room");
    }

    #[test]
    fn parse_chromecasts_skips_line_missing_fn() {
        // No fn= in TXT → skipped.
        let line = "=;eth0;IPv4;Cast;_googlecast._tcp;local;host.local;1.2.3.4;8009;\
                    \"id=abc123\" \"md=Chromecast\"";
        assert_eq!(parse_chromecasts(line).len(), 0);
    }

    #[test]
    fn parse_chromecasts_skips_line_missing_id() {
        // No id= in TXT → skipped.
        let line = "=;eth0;IPv4;Cast;_googlecast._tcp;local;host.local;1.2.3.4;8009;\
                    \"fn=My Cast\" \"md=Chromecast\"";
        assert_eq!(parse_chromecasts(line).len(), 0);
    }

    #[test]
    fn parse_chromecasts_garbage_input_returns_empty() {
        assert_eq!(parse_chromecasts("not;enough;fields").len(), 0);
        assert_eq!(parse_chromecasts("").len(), 0);
        assert_eq!(parse_chromecasts("garbage line here").len(), 0);
    }

    #[test]
    fn parse_chromecasts_invalid_port_skipped() {
        let line =
            "=;eth0;IPv4;Cast;_googlecast._tcp;local;host.local;1.2.3.4;notaport;\"fn=TV\" \"id=abc\"";
        assert_eq!(parse_chromecasts(line).len(), 0);
    }

    #[test]
    fn parse_chromecasts_multiple_devices() {
        let input = format!(
            "{CHROMECAST_SAMPLE}\n\
             =;eth0;IPv4;Cast2;_googlecast._tcp;local;c2.local;10.0.0.2;8009;\
             \"fn=Kitchen\" \"id=kitchen123\""
        );
        let devices = parse_chromecasts(&input);
        assert_eq!(devices.len(), 2);
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Guest TV"));
        assert!(names.contains(&"Kitchen"));
    }

    // ── Soundcard sinks ───────────────────────────────────────────────────────

    #[test]
    fn parse_soundcard_sinks_returns_hardware_sinks() {
        let devices = parse_soundcard_sinks(PACTL_SAMPLE);
        assert_eq!(devices.len(), 2);
        let names: Vec<_> = devices.iter().map(|d| d.id.as_str()).collect();
        assert!(names.contains(&"alsa_output.usb-FiiO_BTR3-00.analog-stereo"));
        assert!(names.contains(&"alsa_output.pci-0000_00_1f.3.analog-stereo"));
    }

    #[test]
    fn parse_soundcard_sinks_excludes_soundsync_prefix() {
        let names: Vec<_> = parse_soundcard_sinks(PACTL_SAMPLE)
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert!(!names.iter().any(|n| n.starts_with("soundsync")));
    }

    #[test]
    fn parse_soundcard_sinks_excludes_effect_sinks() {
        let input = "0\tsoundsync-effect_eq\tmodule-null-sink\ts16le 2ch 48000Hz\tIDLE\n\
                     1\tany_effect_chain\tmodule-alsa-card\ts16le 2ch\tSUSPENDED\n";
        assert_eq!(parse_soundcard_sinks(input).len(), 0);
    }

    #[test]
    fn parse_soundcard_sinks_excludes_monitors() {
        let input = "0\tsoundsync-capture.monitor\tmodule-null-sink\ts16le 2ch 48000Hz\tIDLE\n";
        assert_eq!(parse_soundcard_sinks(input).len(), 0);
    }

    #[test]
    fn parse_soundcard_sinks_garbage_returns_empty() {
        assert_eq!(parse_soundcard_sinks("").len(), 0);
        assert_eq!(parse_soundcard_sinks("no tabs here").len(), 0);
    }

    #[test]
    fn parse_soundcard_sinks_device_fields() {
        let devices = parse_soundcard_sinks(PACTL_SAMPLE);
        for d in &devices {
            assert_eq!(d.kind, OutputKind::Soundcard);
            assert!(d.addr.is_none());
            assert!(d.port.is_none());
        }
    }

    // ── RAOP PipeWire sinks (parse_raop_sinks) ────────────────────────────────

    #[test]
    fn parse_raop_sinks_extracts_all_three_plan_samples() {
        let devices = parse_raop_sinks(RAOP_PACTL_SAMPLE);
        // HP-L, Lounge, Gym, Bose-cba4425d-abcd → 4 RAOP sinks; non-raop excluded.
        assert_eq!(devices.len(), 4);
        let names: Vec<_> = devices.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"HP-L"), "expected HP-L");
        assert!(names.contains(&"Lounge"), "expected Lounge");
        assert!(names.contains(&"Gym"), "expected Gym");
        assert!(names.contains(&"Bose-cba4425d-abcd"), "expected Bose");
    }

    #[test]
    fn parse_raop_sinks_id_is_full_sink_name() {
        let devices = parse_raop_sinks(RAOP_PACTL_SAMPLE);
        let hp = devices.iter().find(|d| d.name == "HP-L").unwrap();
        assert_eq!(hp.id, "raop_sink.HP-L.local.192.168.1.107.7000");
    }

    #[test]
    fn parse_raop_sinks_kind_is_airplay() {
        let devices = parse_raop_sinks(RAOP_PACTL_SAMPLE);
        for d in &devices {
            assert_eq!(d.kind, OutputKind::Airplay);
        }
    }

    #[test]
    fn parse_raop_sinks_addr_and_port_are_none() {
        // Routing uses the sink name directly; addr/port are not extracted.
        let devices = parse_raop_sinks(RAOP_PACTL_SAMPLE);
        for d in &devices {
            assert!(d.addr.is_none());
            assert!(d.port.is_none());
        }
    }

    #[test]
    fn parse_raop_sinks_excludes_non_raop_sinks() {
        let input = "0\talsa_output.usb-FiiO\tmodule-alsa-card\ts32le 2ch\tSUSPENDED\n\
                     1\tsoundsync-capture\tmodule-null-sink\ts16le 2ch\tIDLE\n";
        assert_eq!(parse_raop_sinks(input).len(), 0);
    }

    #[test]
    fn parse_raop_sinks_friendly_name_fallback_when_no_local_suffix() {
        // A hypothetical sink name with no ".local" → use the whole remainder.
        let input = "0\traop_sink.Unusual-Name\tmodule-raop-sink\ts16le 2ch\tSUSPENDED\n";
        let devices = parse_raop_sinks(input);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Unusual-Name");
        assert_eq!(devices[0].id, "raop_sink.Unusual-Name");
    }

    #[test]
    fn parse_raop_sinks_garbage_returns_empty() {
        assert_eq!(parse_raop_sinks("").len(), 0);
        assert_eq!(parse_raop_sinks("no tabs here").len(), 0);
        assert_eq!(parse_raop_sinks("garbage\tline").len(), 0);
    }

    #[test]
    fn parse_raop_sinks_mixed_raop_and_soundcard_lines() {
        // soundcard sinks mixed in — only raop_sink lines should be returned.
        let input = "0\talsa_output.pci\tmodule-alsa-card\ts16le\tSUSPENDED\n\
                     1\traop_sink.TV.local.10.0.0.1.7000\tmodule-raop-sink\ts16le\tIDLE\n\
                     2\tsoundsync-capture.monitor\tmodule-null-sink\ts16le\tIDLE\n";
        let devices = parse_raop_sinks(input);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "TV");
    }

    // ── probe_outputs (sync) ──────────────────────────────────────────────────

    #[test]
    fn probe_outputs_aggregates_all_kinds() {
        // AirPlay now comes from parse_raop_sinks applied to pactl list short sinks
        // (RAOP_PACTL_SAMPLE has 4 raop sinks + 2 non-raop); Chromecast from avahi.
        let runner = FakeCommandRunner::new()
            .on(
                "avahi-browse",
                &["-rtp", "_googlecast._tcp"],
                CommandOutput {
                    status: 0,
                    stdout: CHROMECAST_SAMPLE.to_string(),
                    stderr: String::new(),
                },
            )
            .on(
                "pactl",
                &["list", "short", "sinks"],
                CommandOutput {
                    status: 0,
                    stdout: RAOP_PACTL_SAMPLE.to_string(),
                    stderr: String::new(),
                },
            );

        let avail = probe_outputs(&runner);
        assert_eq!(avail.chromecast.len(), 1);
        assert_eq!(avail.airplay.len(), 4); // HP-L, Lounge, Gym, Bose
        assert_eq!(avail.soundcard.len(), 1); // alsa_output.usb-FiiO_BTR3-00
    }

    #[test]
    fn probe_outputs_best_effort_on_command_failure() {
        // avahi-browse fails to spawn (chromecast empty); pactl succeeds (soundcard+airplay).
        let runner = FakeCommandRunner::new().with_error("avahi-browse").on(
            "pactl",
            &["list", "short", "sinks"],
            CommandOutput {
                status: 0,
                stdout: PACTL_SAMPLE.to_string(),
                stderr: String::new(),
            },
        );

        let avail = probe_outputs(&runner);
        // Failed avahi-browse → chromecast empty (best-effort).
        assert_eq!(avail.chromecast.len(), 0);
        // PACTL_SAMPLE has no raop sinks → airplay empty.
        assert_eq!(avail.airplay.len(), 0);
        assert_eq!(avail.soundcard.len(), 2);
    }

    // ── dedup_by helper ───────────────────────────────────────────────────────

    fn make_device(kind: OutputKind, id: &str, name: &str) -> OutputDevice {
        OutputDevice {
            kind,
            id: id.to_string(),
            name: name.to_string(),
            addr: None,
            port: None,
        }
    }

    #[test]
    fn dedup_by_order_preserving_first_wins() {
        // Three entries: keys "a", "b", "a" — second "a" is a duplicate.
        let devices = vec![
            make_device(OutputKind::Soundcard, "a", "First-A"),
            make_device(OutputKind::Soundcard, "b", "Only-B"),
            make_device(OutputKind::Soundcard, "a", "Second-A"),
        ];
        let result = dedup_by(devices, |d| d.id.clone());
        assert_eq!(result.len(), 2);
        // First-A kept, Second-A dropped; order preserved.
        assert_eq!(result[0].name, "First-A");
        assert_eq!(result[1].name, "Only-B");
    }

    #[test]
    fn dedup_by_all_unique_keys_keeps_all() {
        let devices = vec![
            make_device(OutputKind::Soundcard, "x", "X"),
            make_device(OutputKind::Soundcard, "y", "Y"),
            make_device(OutputKind::Soundcard, "z", "Z"),
        ];
        let result = dedup_by(devices, |d| d.id.clone());
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn dedup_by_empty_input_returns_empty() {
        let result = dedup_by(vec![], |d: &OutputDevice| d.id.clone());
        assert!(result.is_empty());
    }

    // ── Chromecast IPv4/IPv6 dedup ────────────────────────────────────────────

    /// avahi returns both an IPv4 and an IPv6 line for the same Chromecast.
    /// Both lines share the same `id=` TXT value — after dedup only one survives.
    /// parse_chromecasts sorts IPv4 entries first, so dedup keeps the IPv4 address.
    #[test]
    fn parse_chromecasts_dedup_same_id_ipv4_ipv6() {
        let ipv4 = concat!(
            "=;wlp2s0;IPv4;Chromecast-HD-d122;_googlecast._tcp;local;",
            "d122.local;192.168.1.10;8009;",
            "\"fn=Guest TV\" \"id=d122a7025b65\""
        );
        let ipv6 = concat!(
            "=;wlp2s0;IPv6;Chromecast-HD-d122;_googlecast._tcp;local;",
            "d122.local;fe80::1;8009;",
            "\"fn=Guest TV\" \"id=d122a7025b65\""
        );
        let input = format!("{ipv4}\n{ipv6}");
        // Raw parse gives 2 entries with the same id; IPv4 entry is sorted first.
        let raw = parse_chromecasts(&input);
        assert_eq!(raw.len(), 2, "raw should have both lines");
        // IPv4-first sort: first entry should carry the IPv4 address.
        assert_eq!(
            raw[0].addr.as_deref(),
            Some("192.168.1.10"),
            "IPv4 first after sort"
        );

        // After dedup by id → 1; the IPv4 address is retained.
        let deduped = dedup_by(raw, |d| d.id.clone());
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].name, "Guest TV");
        assert_eq!(deduped[0].id, "d122a7025b65");
        // IPv4 address is kept (not the IPv6 fe80:: address).
        assert_eq!(deduped[0].addr.as_deref(), Some("192.168.1.10"));
        assert_eq!(deduped[0].port, Some(8009));
    }

    // ── T3: IPv4 preference in discovery ─────────────────────────────────────

    /// IPv4-only device: addr is the IPv4 address.
    #[test]
    fn parse_chromecasts_ipv4_only_addr_is_ipv4() {
        let line = concat!(
            "=;wlp2s0;IPv4;Cast1;_googlecast._tcp;local;",
            "c1.local;192.168.1.129;8009;",
            "\"fn=Living Room\" \"id=aabbcc\""
        );
        let devices = parse_chromecasts(line);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].addr.as_deref(), Some("192.168.1.129"));
        assert_eq!(devices[0].port, Some(8009));
    }

    /// IPv6-only device: addr falls back to the IPv6 address (still usable
    /// for TLS connect on dual-stack hosts).
    #[test]
    fn parse_chromecasts_ipv6_only_addr_is_ipv6() {
        let line = concat!(
            "=;wlp2s0;IPv6;Cast1;_googlecast._tcp;local;",
            "c1.local;fe80::dead:beef;8009;",
            "\"fn=Living Room\" \"id=aabbcc\""
        );
        let devices = parse_chromecasts(line);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].addr.as_deref(), Some("fe80::dead:beef"));
    }

    /// IPv6 line appears BEFORE IPv4 in the avahi output — IPv4 still wins
    /// because parse_chromecasts sorts IPv4 entries first.
    #[test]
    fn parse_chromecasts_ipv6_before_ipv4_still_prefers_ipv4() {
        let ipv6_first = concat!(
            "=;wlp2s0;IPv6;Cast;_googlecast._tcp;local;",
            "c.local;fe80::1;8009;",
            "\"fn=Kitchen\" \"id=ccddee\""
        );
        let ipv4_second = concat!(
            "=;wlp2s0;IPv4;Cast;_googlecast._tcp;local;",
            "c.local;192.168.1.50;8009;",
            "\"fn=Kitchen\" \"id=ccddee\""
        );
        // IPv6 is listed first in the raw avahi output.
        let input = format!("{ipv6_first}\n{ipv4_second}");
        let deduped = dedup_by(parse_chromecasts(&input), |d| d.id.clone());
        assert_eq!(deduped.len(), 1);
        // IPv4 must be chosen even though IPv6 appeared first.
        assert_eq!(deduped[0].addr.as_deref(), Some("192.168.1.50"));
    }

    /// addr field is the actual IP address, not the .local hostname.
    #[test]
    fn parse_chromecasts_addr_is_ip_not_hostname() {
        let line = concat!(
            "=;wlp2s0;IPv4;Cast;_googlecast._tcp;local;",
            "cast-device.local;10.0.1.99;8009;",
            "\"fn=Office\" \"id=112233\""
        );
        let devices = parse_chromecasts(line);
        assert_eq!(devices.len(), 1);
        // addr should be the IP (field 7), not the hostname (field 6).
        assert_eq!(devices[0].addr.as_deref(), Some("10.0.1.99"));
        assert_ne!(devices[0].addr.as_deref(), Some("cast-device.local"));
    }

    /// Two distinct Chromecasts with different ids are both preserved.
    #[test]
    fn parse_chromecasts_dedup_different_ids_kept() {
        let line1 = concat!(
            "=;wlp2s0;IPv4;Cast1;_googlecast._tcp;local;",
            "c1.local;192.168.1.10;8009;",
            "\"fn=Living Room\" \"id=aabbcc\""
        );
        let line2 = concat!(
            "=;wlp2s0;IPv4;Cast2;_googlecast._tcp;local;",
            "c2.local;192.168.1.11;8009;",
            "\"fn=Kitchen\" \"id=ddeeff\""
        );
        let input = format!("{line1}\n{line2}");
        let deduped = dedup_by(parse_chromecasts(&input), |d| d.id.clone());
        assert_eq!(deduped.len(), 2);
        let names: Vec<_> = deduped.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Living Room"));
        assert!(names.contains(&"Kitchen"));
    }

    // ── AirPlay (RAOP) name dedup ─────────────────────────────────────────────

    /// Two RAOP sinks with the same friendly name (bridge duplicates) collapse to one.
    #[test]
    fn raop_dedup_same_name_collapses_to_one() {
        // Simulates two bridge entries that resolve to the same friendly name.
        let input = "\
0\traop_sink.a0d7b954-aircast.local.192.168.1.20.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
1\traop_sink.a0d7b954-aircast.local.192.168.1.21.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
";
        let raw = parse_raop_sinks(input);
        assert_eq!(raw.len(), 2, "raw should see both sink lines");
        assert_eq!(raw[0].name, raw[1].name, "both have same friendly name");

        let deduped = dedup_by(raw, |d| d.name.clone());
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].name, "a0d7b954-aircast");
    }

    /// Real devices with distinct friendly names are all kept after dedup.
    #[test]
    fn raop_dedup_distinct_names_all_kept() {
        let deduped = dedup_by(parse_raop_sinks(RAOP_PACTL_SAMPLE), |d| d.name.clone());
        // HP-L, Lounge, Gym, Bose-cba4425d-abcd — all distinct.
        assert_eq!(deduped.len(), 4);
        let names: Vec<_> = deduped.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"HP-L"));
        assert!(names.contains(&"Lounge"));
        assert!(names.contains(&"Gym"));
        assert!(names.contains(&"Bose-cba4425d-abcd"));
    }

    /// Mix: two bridge duplicates + two real devices → three entries total.
    #[test]
    fn raop_dedup_mixed_bridge_and_real() {
        let input = "\
0\traop_sink.HP-L.local.192.168.1.107.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
1\traop_sink.a0d7b954-aircast.local.192.168.1.20.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
2\traop_sink.a0d7b954-aircast.local.192.168.1.21.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
3\traop_sink.Gym.local.192.168.1.115.7000\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED
";
        let deduped = dedup_by(parse_raop_sinks(input), |d| d.name.clone());
        assert_eq!(deduped.len(), 3); // HP-L, a0d7b954-aircast (once), Gym
        let names: Vec<_> = deduped.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"HP-L"));
        assert!(names.contains(&"a0d7b954-aircast"));
        assert!(names.contains(&"Gym"));
    }

    // ── parse_module_id_by_name ───────────────────────────────────────────────

    const MODULES_SAMPLE: &str = "\
7\tmodule-always-sink\t\n\
8\tmodule-null-sink\tsink_name=soundsync-capture\n\
42\tmodule-raop-discover\t\n\
99\tmodule-loopback\tsource=soundsync-capture.monitor sink=raop_sink.HP-L.local.192.168.1.107.7000\n\
";

    #[test]
    fn parse_module_id_by_name_finds_raop_discover() {
        let id = parse_module_id_by_name(MODULES_SAMPLE, "module-raop-discover");
        assert_eq!(id, Some(42));
    }

    #[test]
    fn parse_module_id_by_name_absent_returns_none() {
        let id = parse_module_id_by_name(MODULES_SAMPLE, "module-does-not-exist");
        assert_eq!(id, None);
    }

    #[test]
    fn parse_module_id_by_name_empty_input_returns_none() {
        assert_eq!(parse_module_id_by_name("", "module-raop-discover"), None);
    }

    #[test]
    fn parse_module_id_by_name_garbage_lines_skipped() {
        // Lines with no tab, unparseable ids, or too few fields — all skipped safely.
        let garbage = "not a valid line\n\
                       \t\t\n\
                       notanumber\tmodule-raop-discover\t\n\
                       garbage here\n";
        // The entry with unparseable id returns None even though name matches.
        assert_eq!(
            parse_module_id_by_name(garbage, "module-raop-discover"),
            None
        );
    }

    #[test]
    fn parse_module_id_by_name_first_match_wins() {
        // Two rows with the same module name — first one's id is returned.
        let input = "10\tmodule-raop-discover\t\n\
                     20\tmodule-raop-discover\tsome-args\n";
        assert_eq!(
            parse_module_id_by_name(input, "module-raop-discover"),
            Some(10)
        );
    }

    #[test]
    fn parse_module_id_by_name_no_partial_match() {
        // Name must be an exact match, not a prefix/substring.
        let input = "5\tmodule-raop-discover-extra\t\n";
        assert_eq!(parse_module_id_by_name(input, "module-raop-discover"), None);
    }
}
