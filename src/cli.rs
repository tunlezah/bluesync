//! Command-line argument parsing. Hand-rolled (no new deps) — the binary has a
//! small fixed set of subcommands; see `parse_args` for dispatch rules.

/// Top-level subcommand dispatched by `main`.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Run the daemon (default when no subcommand is given).
    Run,
    /// Self-check: verify binaries, PipeWire, WP config, adapter, web dir.
    Doctor,
    /// Write the version-correct WirePlumber A2DP config (run as root at install).
    ApplyWpConfig,
    /// Set the Bluetooth adapter Class-of-Device + HCI name (run as root at boot).
    ApplyAdapterConfig,
    /// Print the version string and exit.
    Version,
    /// Print usage and exit (also shown on unknown subcommands).
    Help,
}

/// Parse the arguments that come **after** the program name.
///
/// - Empty → `Run`
/// - `"doctor"` → `Doctor`
/// - `"apply-wireplumber-config"` → `ApplyWpConfig`
/// - `"apply-adapter-config"` → `ApplyAdapterConfig`
/// - `"version"` | `"--version"` | `"-V"` → `Version`
/// - `"help"` | `"--help"` | `"-h"` → `Help`
/// - Anything else → `Help` (unknown subcommand)
pub fn parse_args(args: &[String]) -> Command {
    match args.first().map(String::as_str) {
        None => Command::Run,
        Some("doctor") => Command::Doctor,
        Some("apply-wireplumber-config") => Command::ApplyWpConfig,
        Some("apply-adapter-config") => Command::ApplyAdapterConfig,
        Some("version") | Some("--version") | Some("-V") => Command::Version,
        Some("help") | Some("--help") | Some("-h") => Command::Help,
        Some(_) => Command::Help,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_args_is_run() {
        assert_eq!(parse_args(&[]), Command::Run);
    }

    #[test]
    fn doctor_subcommand() {
        assert_eq!(parse_args(&args(&["doctor"])), Command::Doctor);
    }

    #[test]
    fn apply_wireplumber_config_subcommand() {
        assert_eq!(
            parse_args(&args(&["apply-wireplumber-config"])),
            Command::ApplyWpConfig
        );
    }

    #[test]
    fn apply_adapter_config_subcommand() {
        assert_eq!(
            parse_args(&args(&["apply-adapter-config"])),
            Command::ApplyAdapterConfig
        );
    }

    #[test]
    fn version_aliases() {
        assert_eq!(parse_args(&args(&["version"])), Command::Version);
        assert_eq!(parse_args(&args(&["--version"])), Command::Version);
        assert_eq!(parse_args(&args(&["-V"])), Command::Version);
    }

    #[test]
    fn help_aliases() {
        assert_eq!(parse_args(&args(&["help"])), Command::Help);
        assert_eq!(parse_args(&args(&["--help"])), Command::Help);
        assert_eq!(parse_args(&args(&["-h"])), Command::Help);
    }

    #[test]
    fn unknown_subcommand_is_help() {
        assert_eq!(parse_args(&args(&["unknown-command"])), Command::Help);
        assert_eq!(parse_args(&args(&["bogus"])), Command::Help);
        assert_eq!(parse_args(&args(&["--foo"])), Command::Help);
    }
}
