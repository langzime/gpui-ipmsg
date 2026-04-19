use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserConfig {
    pub username: String,
    pub group: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub user: UserConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            user: UserConfig {
                username: whoami::username(),
                group: "自己".to_string(),
            },
        }
    }
}

pub fn get_config_path() -> PathBuf {
    app_config_dir().join("config.toml")
}

pub fn app_config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("gpui-ipmsg");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("gpui-ipmsg");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join("gpui-ipmsg");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config").join("gpui-ipmsg");
        }
    }
    PathBuf::from(".").join("gpui-ipmsg")
}

fn legacy_config_path() -> PathBuf {
    PathBuf::from(".").join("config.toml")
}

fn read_config(path: &PathBuf) -> Option<AppConfig> {
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(path).ok()?;
    toml::from_str::<AppConfig>(&content).ok()
}

pub fn load_config() -> AppConfig {
    let path = get_config_path();
    if let Some(config) = read_config(&path) {
        return config;
    }

    let legacy = legacy_config_path();
    if let Some(config) = read_config(&legacy) {
        let _ = save_config(&config);
        return config;
    }

    let config = AppConfig::default();
    let _ = save_config(&config);
    config
}

pub fn save_config(config: &AppConfig) -> std::io::Result<()> {
    let content = toml::to_string(config).map_err(std::io::Error::other)?;
    let path = get_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}
