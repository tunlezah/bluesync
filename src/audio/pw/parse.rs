//! Parsers for `pactl list modules` and `wpctl status` output.

/// Find the module id of a `module-null-sink` whose argument mentions `sink_name`,
/// by scanning `pactl list modules` output.
pub fn parse_null_sink_module_id(pactl_output: &str, sink_name: &str) -> Option<u32> {
    let mut current_id: Option<u32> = None;
    let mut is_null_sink = false;
    for line in pactl_output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Module #") {
            current_id = rest.parse::<u32>().ok();
            is_null_sink = false;
        } else if trimmed.starts_with("Name:") {
            is_null_sink = trimmed.contains("module-null-sink");
        } else if trimmed.starts_with("Argument:") && is_null_sink && trimmed.contains(sink_name) {
            return current_id;
        }
    }
    None
}

/// Find ALL `module-null-sink` module ids whose argument contains
/// `sink_name=<sink_name>`, by scanning `pactl list short modules` output.
///
/// Unlike [`parse_null_sink_module_id`] (which parses the verbose `pactl list
/// modules` block form and returns only the first match), this parses the short
/// tab-separated form `<id>\t<name>\t<args>` and returns every match. On
/// PipeWire 1.6 duplicate null sinks all carry the IDENTICAL node name
/// `soundsync-capture` (no `.2`/`.3` suffix — old pulse behaviour) and differ
/// only by sink/node id and owning module id, so duplicates can only be
/// identified by the `sink_name=` module argument, never by name (W0.4 spike).
pub fn parse_all_null_sink_module_ids(short_modules_output: &str, sink_name: &str) -> Vec<u32> {
    let needle = format!("sink_name={sink_name}");
    let mut ids = Vec::new();
    for line in short_modules_output.lines() {
        let mut fields = line.split('\t');
        let (Some(id_field), Some(name_field), Some(args_field)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if name_field.trim() != "module-null-sink" {
            continue;
        }
        if !line_contains_node_token(args_field, &needle) {
            continue;
        }
        if let Ok(id) = id_field.trim().parse::<u32>() {
            ids.push(id);
        }
    }
    ids
}

/// A node-name character: matching must not be flanked by these, so that a
/// search for `soundsync-capture` does not match `soundsync-capture.monitor`
/// (a source) or `soundsync-capture-source` (the pw-loopback variant).
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '.' || c == '-' || c == '_'
}

/// True if `name` appears in `line` as a whole token (not as a substring of a
/// longer node name like `<name>.monitor`).
fn line_contains_node_token(line: &str, name: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = line[start..].find(name) {
        let abs = start + pos;
        let before = line[..abs].chars().next_back();
        let after = line[abs + name.len()..].chars().next();
        if before.is_none_or(|c| !is_name_char(c)) && after.is_none_or(|c| !is_name_char(c)) {
            return true;
        }
        start = abs + name.len();
    }
    false
}

/// Find a node id by name in `wpctl status` output (the number before the dot).
/// Matches the node name as a whole token so the auto-created `.monitor` source
/// is never mistaken for the sink.
pub fn find_wpctl_node_id(status_output: &str, node_name: &str) -> Option<u32> {
    for line in status_output.lines() {
        if line_contains_node_token(line, node_name) {
            let trimmed = line
                .trim()
                .trim_start_matches(|c: char| !c.is_ascii_digit());
            if let Some(dot_pos) = trimmed.find('.') {
                if let Ok(id) = trimmed[..dot_pos].trim().parse::<u32>() {
                    return Some(id);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpctl_node_id_found_among_lines() {
        let output = "\
Audio
 ├─ Sinks:
 │  *   42. Built-in Audio Analog Stereo [vol: 1.00]
 │      40. soundsync-capture            [vol: 1.00]
";
        assert_eq!(find_wpctl_node_id(output, "soundsync-capture"), Some(40));
    }

    #[test]
    fn wpctl_node_id_ignores_monitor_and_source_variants() {
        // The .monitor source appears FIRST and must NOT be matched; the real
        // sink (token match) is returned instead.
        let output = "\
 ├─ Sources:
 │      55. soundsync-capture.monitor     [vol: 1.00]
 ├─ Sinks:
 │      40. soundsync-capture             [vol: 1.00]
";
        assert_eq!(find_wpctl_node_id(output, "soundsync-capture"), Some(40));
        // A pw-loopback '-source' variant alone is not a match either.
        assert_eq!(
            find_wpctl_node_id(
                " │  55. soundsync-capture-source [vol: 1.00]\n",
                "soundsync-capture"
            ),
            None
        );
    }

    #[test]
    fn wpctl_node_id_with_default_star() {
        assert_eq!(
            find_wpctl_node_id(
                " │  *   40. soundsync-capture [vol: 0.50]\n",
                "soundsync-capture"
            ),
            Some(40)
        );
    }

    #[test]
    fn wpctl_node_id_not_found() {
        assert_eq!(
            find_wpctl_node_id(" │  42. Built-in Audio [vol: 1.00]\n", "soundsync-capture"),
            None
        );
    }

    #[test]
    fn null_sink_module_id_found() {
        let output = "\
Module #10
	Name: module-something-else
	Argument: foo=bar
Module #23
	Name: module-null-sink
	Argument: sink_name=soundsync-capture sink_properties=device.description=SoundSync-Capture
";
        assert_eq!(
            parse_null_sink_module_id(output, "soundsync-capture"),
            Some(23)
        );
    }

    #[test]
    fn null_sink_module_id_ignores_non_null_sink_and_other_names() {
        let output = "\
Module #10
	Name: module-null-sink
	Argument: sink_name=other-sink
Module #11
	Name: module-loopback
	Argument: sink=soundsync-capture
";
        assert_eq!(parse_null_sink_module_id(output, "soundsync-capture"), None);
    }

    #[test]
    fn all_null_sink_ids_matches_every_duplicate_by_arg_not_name() {
        // `pactl list short modules` form: <id>\t<name>\t<args>. Two duplicate
        // soundsync-capture null sinks (identical node name on PW 1.6) plus
        // unrelated modules that must be ignored.
        let output = "\
10\tmodule-something-else\tfoo=bar
536870917\tmodule-null-sink\tsink_name=soundsync-capture sink_properties=device.description=SoundSync-Capture
536870918\tmodule-null-sink\tsink_name=soundsync-capture sink_properties=device.description=SoundSync-Capture
22\tmodule-null-sink\tsink_name=other-sink
23\tmodule-loopback\tsink=soundsync-capture
";
        assert_eq!(
            parse_all_null_sink_module_ids(output, "soundsync-capture"),
            vec![536870917, 536870918]
        );
    }

    #[test]
    fn all_null_sink_ids_zero_and_one_matches() {
        // Zero matches.
        assert_eq!(
            parse_all_null_sink_module_ids(
                "7\tmodule-null-sink\tsink_name=other\n",
                "soundsync-capture"
            ),
            Vec::<u32>::new()
        );
        // Exactly one match.
        assert_eq!(
            parse_all_null_sink_module_ids(
                "7\tmodule-null-sink\tsink_name=soundsync-capture sink_properties=device.description=SoundSync-Capture\n",
                "soundsync-capture"
            ),
            vec![7]
        );
    }

    #[test]
    fn all_null_sink_ids_does_not_match_longer_sink_name_prefix() {
        // A null sink whose name merely starts with the target must NOT match.
        let output = "\
7\tmodule-null-sink\tsink_name=soundsync-capture-other
8\tmodule-null-sink\tsink_name=soundsync-capture
";
        assert_eq!(
            parse_all_null_sink_module_ids(output, "soundsync-capture"),
            vec![8]
        );
    }
}
