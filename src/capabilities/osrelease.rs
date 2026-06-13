//! Minimal /etc/os-release parser (distro id + version).

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OsRelease {
    pub id: Option<String>,
    pub version_id: Option<String>,
}

impl OsRelease {
    /// Parse `ID`/`VERSION_ID` from os-release content. Strips surrounding
    /// double or single quotes (both are permitted by the freedesktop spec).
    pub fn parse(content: &str) -> OsRelease {
        let mut os = OsRelease::default();
        for line in content.lines() {
            let line = line.trim();
            if let Some((k, v)) = line.split_once('=') {
                let val = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                match k.trim() {
                    "ID" => os.id = Some(val),
                    "VERSION_ID" => os.version_id = Some(val),
                    _ => {}
                }
            }
        }
        os
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const UBUNTU_2404: &str =
        "PRETTY_NAME=\"Ubuntu 24.04.4 LTS\"\nID=ubuntu\nVERSION_ID=\"24.04\"\n";
    #[test]
    fn parses_id_and_version() {
        let os = OsRelease::parse(UBUNTU_2404);
        assert_eq!(os.id.as_deref(), Some("ubuntu"));
        assert_eq!(os.version_id.as_deref(), Some("24.04"));
    }
    #[test]
    fn strips_single_quotes() {
        let os = OsRelease::parse("ID='arch'\nVERSION_ID='rolling'\n");
        assert_eq!(os.id.as_deref(), Some("arch"));
        assert_eq!(os.version_id.as_deref(), Some("rolling"));
    }
    #[test]
    fn missing_fields_are_none() {
        let os = OsRelease::parse("FOO=bar\n");
        assert_eq!(os.id, None);
        assert_eq!(os.version_id, None);
    }
}
