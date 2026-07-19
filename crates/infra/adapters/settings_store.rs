use std::fs;
use std::path::PathBuf;

use super::app_config_file::{
    self, config_file_path, get_config_dir as app_config_dir, render_config_file, write_config_file,
};
use crate::app::model::shared::theme_id::ThemeId;
use crate::app::ports::outbound::{AppSettings, SettingsStore, SettingsStoreError};
use crate::config::connection_config::{CURRENT_VERSION, ConfigVersionCheck, ConnectionConfigFile};

#[cfg(test)]
use super::app_config_file::CONFIG_FILE_NAME;

pub struct TomlSettingsStore {
    config_dir: PathBuf,
}

impl TomlSettingsStore {
    pub fn new() -> Result<Self, SettingsStoreError> {
        let config_dir = get_config_dir()?;
        Ok(Self { config_dir })
    }

    pub fn with_config_dir(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    fn config_file_path(&self) -> PathBuf {
        config_file_path(&self.config_dir)
    }

    fn load_config_file(&self) -> Result<Option<ConnectionConfigFile>, SettingsStoreError> {
        let path = self.config_file_path();
        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)?;
        let version_check: ConfigVersionCheck = toml::from_str(&content)?;

        if version_check.version != CURRENT_VERSION {
            return Err(SettingsStoreError::VersionMismatch {
                found: version_check.version,
                expected: CURRENT_VERSION,
            });
        }

        Ok(Some(toml::from_str::<ConnectionConfigFile>(&content)?))
    }
}

impl SettingsStore for TomlSettingsStore {
    fn load(&self) -> Result<AppSettings, SettingsStoreError> {
        self.load_config_file()?
            .map_or_else(|| Ok(AppSettings::default()), app_settings)
    }

    fn save(&self, settings: AppSettings) -> Result<(), SettingsStoreError> {
        let _guard = app_config_file::lock();

        let mut config = self
            .load_config_file()?
            .unwrap_or_else(|| ConnectionConfigFile {
                version: CURRENT_VERSION,
                theme: None,
                connections: vec![],
            });
        set_app_settings(&mut config, settings);
        let content = toml::to_string_pretty(&config)?;
        let content_with_header = render_config_file(&content);
        write_config_file(&self.config_dir, &content_with_header)?;

        Ok(())
    }
}

fn get_config_dir() -> Result<PathBuf, SettingsStoreError> {
    Ok(app_config_dir()?)
}

fn app_settings(config: ConnectionConfigFile) -> Result<AppSettings, SettingsStoreError> {
    let theme_id = match config.theme.as_deref() {
        None => ThemeId::default(),
        Some(value) => ThemeId::from_config_value(value)
            .ok_or_else(|| SettingsStoreError::UnknownTheme(value.to_string()))?,
    };
    Ok(AppSettings { theme_id })
}

fn set_app_settings(config: &mut ConnectionConfigFile, settings: AppSettings) {
    config.theme = Some(settings.theme_id.config_value().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::shared::theme_id::ThemeId;
    use tempfile::TempDir;

    #[test]
    fn missing_file_returns_default_settings() {
        let temp_dir = TempDir::new().unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        let settings = store.load().unwrap();

        assert_eq!(settings.theme_id, ThemeId::Default);
    }

    #[test]
    fn save_and_load_round_trips_theme() {
        let temp_dir = TempDir::new().unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        store
            .save(AppSettings {
                theme_id: ThemeId::Light,
            })
            .unwrap();

        let settings = store.load().unwrap();
        assert_eq!(settings.theme_id, ThemeId::Light);
    }

    #[test]
    fn save_preserves_existing_connections() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join(CONFIG_FILE_NAME),
            r#"version = 2

[[connections]]
id = "test-id"
name = "Test"
host = "localhost"
port = 5432
database = "testdb"
username = "testuser"
password = "testpass"
ssl_mode = "prefer"
"#,
        )
        .unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        store
            .save(AppSettings {
                theme_id: ThemeId::Light,
            })
            .unwrap();

        let content = fs::read_to_string(temp_dir.path().join(CONFIG_FILE_NAME)).unwrap();
        assert!(content.contains("theme = \"light\""));
        assert!(content.contains("[[connections]]"));
        assert!(content.contains("name = \"Test\""));
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(CONFIG_FILE_NAME), "not = [").unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        let result = store.load();

        assert!(matches!(
            result,
            Err(SettingsStoreError::TomlDeserialize(_))
        ));
    }

    #[test]
    fn load_version_mismatch_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join(CONFIG_FILE_NAME),
            "version = 1\nconnections = []\n",
        )
        .unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        let result = store.load();

        assert!(matches!(
            result,
            Err(SettingsStoreError::VersionMismatch {
                found: 1,
                expected: CURRENT_VERSION,
            })
        ));
    }

    #[test]
    fn load_unknown_theme_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join(CONFIG_FILE_NAME),
            "version = 2\ntheme = \"terminal\"\nconnections = []\n",
        )
        .unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        let result = store.load();

        assert!(matches!(
            result,
            Err(SettingsStoreError::UnknownTheme(theme)) if theme == "terminal"
        ));
    }

    #[test]
    fn missing_theme_falls_back_to_default() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(
            temp_dir.path().join(CONFIG_FILE_NAME),
            "version = 2\nconnections = []\n",
        )
        .unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        let settings = store.load().unwrap();

        assert_eq!(settings.theme_id, ThemeId::Default);
    }

    #[test]
    fn save_invalid_toml_returns_error_without_overwriting_file() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "not = [").unwrap();
        let store = TomlSettingsStore::with_config_dir(temp_dir.path().to_path_buf());

        let result = store.save(AppSettings {
            theme_id: ThemeId::Light,
        });

        assert!(matches!(
            result,
            Err(SettingsStoreError::TomlDeserialize(_))
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "not = [");
    }
}
