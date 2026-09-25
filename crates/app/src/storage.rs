use anyhow::{Context, Result};
use easy_switch_core::config::Config;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Paths {
    pub root: PathBuf,
}
impl Paths {
    pub fn new(override_path: Option<PathBuf>) -> Result<Self> {
        let root = match override_path {
            Some(path) => path,
            None => directories::ProjectDirs::from("", "", "easy-switch-link")
                .context("No user configuration directory")?
                .config_dir()
                .to_path_buf(),
        };
        let root = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()?.join(root)
        };
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { root })
    }
    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn load(&self) -> Result<Config> {
        match std::fs::read_to_string(self.config()) {
            Ok(text) => {
                let config: Config = toml::from_str(&text).context("Invalid config.toml")?;
                let old = config.version == 1;
                let config = config.migrate()?;
                if old {
                    let backup = self.root.join("config-v1.backup.toml");
                    if !backup.exists() {
                        atomic_write(&backup, text.as_bytes())?;
                    }
                    self.save(&config)?;
                }
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, config: &Config) -> Result<()> {
        config.validate()?;
        atomic_write(&self.config(), toml::to_string_pretty(config)?.as_bytes())
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("Missing parent directory")?;
    std::fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}
pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_one_migrates_once_without_enabling_network_or_exporting_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf())).unwrap();
        let old = Config {
            version: 1,
            ..Default::default()
        };
        let text = toml::to_string(&old).unwrap();
        atomic_write(&paths.config(), text.as_bytes()).unwrap();
        atomic_write(
            &paths.root.join("peer-identity.json"),
            b"private-key-canary",
        )
        .unwrap();
        let migrated = paths.load().unwrap();
        assert_eq!(migrated.version, 2);
        assert!(!migrated.follow.enabled);
        assert_eq!(migrated.groups, old.groups);
        assert_eq!(migrated.computers, old.computers);
        assert_eq!(
            std::fs::read_to_string(paths.root.join("config-v1.backup.toml")).unwrap(),
            text
        );
        assert!(
            !toml::to_string(&migrated)
                .unwrap()
                .contains("private-key-canary")
        );
        assert_eq!(paths.load().unwrap(), migrated);
    }
    #[test]
    fn invalid_save_preserves_last_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf())).unwrap();
        let config = Config::default();
        paths.save(&config).unwrap();
        let mut invalid = config.clone();
        invalid.version = 999;
        assert!(paths.save(&invalid).is_err());
        assert_eq!(paths.load().unwrap(), config);
    }
}
