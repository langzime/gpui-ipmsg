use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum LanguageEncoding {
    #[serde(rename = "UTF-8")]
    Utf8,
    #[serde(rename = "GB18030")]
    Gb18030,
}

impl Default for LanguageEncoding {
    fn default() -> Self {
        Self::Gb18030
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum UiLanguage {
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en")]
    En,
}

impl UiLanguage {
    pub fn as_locale(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }
}

fn detect_system_ui_language() -> UiLanguage {
    let locale = sys_locale::get_locale().unwrap_or_default().to_lowercase();
    let normalized = locale.replace('_', "-");
    if normalized.starts_with("zh") {
        UiLanguage::ZhCn
    } else {
        UiLanguage::En
    }
}

impl Default for UiLanguage {
    fn default() -> Self {
        detect_system_ui_language()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserConfig {
    pub username: String,
    pub group: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub user: UserConfig,
    #[serde(default)]
    pub language: LanguageEncoding,
    #[serde(default)]
    pub ui_language: UiLanguage,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            user: UserConfig {
                username: whoami::username(),
                group: "自己".to_string(),
            },
            language: LanguageEncoding::Gb18030,
            ui_language: UiLanguage::default(),
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
