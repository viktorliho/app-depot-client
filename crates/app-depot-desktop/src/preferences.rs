//! Small presentation-only settings file beside the library metadata database.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub theme: Theme,
    pub grid: bool,
    pub last_profile: Option<String>,
    pub ui_scale: f32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            grid: true,
            last_profile: None,
            ui_scale: 1.15,
        }
    }
}

fn path(root: &Path) -> PathBuf {
    root.join("desktop-preferences.json")
}
pub fn load(root: &Path) -> Result<Preferences, String> {
    let file = path(root);
    match fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(e) => Err(e.to_string()),
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err("unsafe desktop preferences file".into());
            }
            let bytes = fs::read(file).map_err(|e| e.to_string())?;
            if bytes.len() > 16 * 1024 {
                return Err("desktop preferences file is too large".into());
            }
            let mut value: Preferences =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            value.ui_scale = if value.ui_scale.is_finite() {
                value.ui_scale.clamp(0.85, 1.6)
            } else {
                1.15
            };
            Ok(value)
        }
    }
}
pub fn save(root: &Path, value: &Preferences) -> Result<(), String> {
    let file = path(root);
    if fs::symlink_metadata(&file).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err("unsafe desktop preferences file".into());
    }
    let tmp = root.join(format!(".desktop-preferences-{}.tmp", uuid::Uuid::new_v4()));
    let mut writer = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    let result = (|| -> Result<(), String> {
        writer
            .write_all(&serde_json::to_vec(value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        writer.sync_all().map_err(|e| e.to_string())?;
        drop(writer);
        fs::rename(&tmp, &file).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().grid);
        assert_eq!(load(dir.path()).unwrap().theme, Theme::System);
        let value = Preferences {
            theme: Theme::Light,
            grid: false,
            last_profile: Some("profile".into()),
            ui_scale: 1.25,
        };
        save(dir.path(), &value).unwrap();
        assert_eq!(
            load(dir.path()).unwrap().last_profile.as_deref(),
            Some("profile")
        );
        assert_eq!(load(dir.path()).unwrap().ui_scale, 1.25);
        let text = fs::read_to_string(path(dir.path())).unwrap();
        assert!(!text.contains("token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path(dir.path())).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_preference_file_is_rejected() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unrelated");
        fs::write(&target, b"keep").unwrap();
        symlink(&target, path(dir.path())).unwrap();
        assert!(load(dir.path()).is_err());
        assert!(save(dir.path(), &Preferences::default()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
    }
    #[test]
    fn old_preferences_and_out_of_range_scale_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            path(dir.path()),
            br#"{"theme":"Dark","grid":false,"last_profile":null}"#,
        )
        .unwrap();
        let old = load(dir.path()).unwrap();
        assert_eq!(old.ui_scale, 1.15);
        assert!(!old.grid);
        fs::write(path(dir.path()), br#"{"ui_scale":100.0}"#).unwrap();
        assert_eq!(load(dir.path()).unwrap().ui_scale, 1.6);
    }
}
