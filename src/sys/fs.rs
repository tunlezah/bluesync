//! Filesystem effects (reading os-release, writing WirePlumber configs).
use std::io;

/// File operations the integration logic needs. Abstracted for testability.
pub trait Fs {
    fn read_to_string(&self, path: &str) -> io::Result<String>;
    /// Atomically write `contents` to `dir/filename` (create `dir` if needed).
    fn write_atomic(&self, dir: &str, filename: &str, contents: &str) -> io::Result<()>;
    /// Remove `dir/filename`; a missing file is treated as success.
    fn remove_file(&self, dir: &str, filename: &str) -> io::Result<()>;
}

/// Real implementation over `std::fs`.
pub struct SystemFs;

impl Fs for SystemFs {
    fn read_to_string(&self, path: &str) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn write_atomic(&self, dir: &str, filename: &str, contents: &str) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = format!("{dir}/.{filename}.tmp");
        let final_path = format!("{dir}/{filename}");
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, &final_path)
    }

    fn remove_file(&self, dir: &str, filename: &str) -> io::Result<()> {
        match std::fs::remove_file(format!("{dir}/{filename}")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("soundsync-test-{}-{}", tag, std::process::id()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn write_then_read_roundtrip_and_remove() {
        let fs = SystemFs;
        let dir = temp_dir("fs");
        fs.write_atomic(&dir, "f.conf", "hello").unwrap();
        let full = format!("{dir}/f.conf");
        assert_eq!(fs.read_to_string(&full).unwrap(), "hello");
        fs.remove_file(&dir, "f.conf").unwrap();
        assert!(fs.read_to_string(&full).is_err());
        // remove of a missing file is a no-op (Ok)
        fs.remove_file(&dir, "f.conf").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
