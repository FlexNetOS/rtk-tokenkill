//! Shared audit-log path and append primitives.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn directory() -> Option<PathBuf> {
    directory_from(
        std::env::var_os("RTK_AUDIT_DIR").map(PathBuf::from),
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
    )
}

fn directory_from(explicit: Option<PathBuf>, xdg_data_home: Option<PathBuf>) -> Option<PathBuf> {
    explicit
        .filter(|value| !value.as_os_str().is_empty())
        .or_else(|| {
            xdg_data_home
                .filter(|value| !value.as_os_str().is_empty())
                .map(|path| path.join("rtk"))
        })
}

pub fn log_path() -> Option<PathBuf> {
    directory().map(|dir| dir.join("hook-audit.log"))
}

pub fn append(line: &str) -> Option<()> {
    let dir = directory()?;
    fs::create_dir_all(&dir).ok()?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("hook-audit.log"))
        .ok()?;
    writeln!(file, "{line}").ok()
}

pub fn probe_writable() -> Result<PathBuf, String> {
    let dir = directory().ok_or_else(|| {
        "set RTK_AUDIT_DIR or XDG_DATA_HOME; implicit ~/.local ownership is disabled".to_string()
    })?;
    fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let probe = dir.join(format!(".audit-probe-{}", std::process::id()));
    let result = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
        .and_then(|mut file| file.write_all(b"rtk-audit-probe\n"));
    let _ = fs::remove_file(&probe);
    match result {
        Ok(()) => Ok(dir),
        Err(error) => Err(format!("cannot append in {}: {error}", dir.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_audit_dir_has_precedence() {
        assert_eq!(
            directory_from(
                Some(PathBuf::from("/tmp/rtk-audit")),
                Some(PathBuf::from("/srv/profile-data"))
            ),
            Some(PathBuf::from("/tmp/rtk-audit"))
        );
    }

    #[test]
    fn xdg_path_shape_is_profile_compatible() {
        assert_eq!(
            directory_from(None, Some(PathBuf::from("/srv/profile-data"))),
            Some(PathBuf::from("/srv/profile-data/rtk"))
        );
        assert_eq!(directory_from(None, None), None);
    }
}
