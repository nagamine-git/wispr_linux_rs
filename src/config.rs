use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use log::{info, warn};
use directories::ProjectDirs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    /// OpenAI API key
    pub api_key: String,
    
    /// Path to save recordings temporarily
    pub temp_dir: PathBuf,
    
    /// Recording settings
    pub recording: RecordingConfig,
    
    /// UI settings
    pub ui: UiConfig,
    
    /// Keyboard shortcut settings
    pub shortcuts: ShortcutConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecordingConfig {
    /// Maximum recording duration in seconds
    pub max_duration_secs: u64,
    
    /// Sample rate for audio recording
    pub sample_rate: u32,
    
    /// Whether to play a sound when recording starts/stops
    pub play_sounds: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UiConfig {
    /// Dark mode preference
    pub dark_mode: bool,
    
    /// Show notifications for transcription
    pub notification_enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ShortcutConfig {
    /// Key combination to start/stop recording
    pub toggle_recording: String,
    
    /// Key combination to clear the transcript
    pub clear_transcript: String,
    
    /// Key combination to copy transcript to clipboard
    pub copy_to_clipboard: String,
    
    /// Automatically paste text after transcription
    pub auto_paste: bool,
}

/// Get the config file path
pub fn get_config_path(custom_path: Option<String>) -> PathBuf {
    if let Some(path) = custom_path {
        return PathBuf::from(path);
    }
    
    if let Some(proj_dirs) = ProjectDirs::from("com", "wispr", "wispr_linux_rs") {
        let config_dir = proj_dirs.config_dir();
        fs::create_dir_all(config_dir).ok();
        config_dir.join("config.toml")
    } else {
        warn!("Could not determine config directory, using current directory");
        PathBuf::from("config.toml")
    }
}

/// Get the temporary directory path
pub fn get_temp_dir() -> PathBuf {
    if let Some(proj_dirs) = ProjectDirs::from("com", "wispr", "wispr_linux_rs") {
        let cache_dir = proj_dirs.cache_dir();
        fs::create_dir_all(cache_dir).ok();
        cache_dir.to_path_buf()
    } else {
        warn!("Could not determine cache directory, using system temp");
        std::env::temp_dir().join("wispr")
    }
}

/// Load configuration from file
pub fn load_config(custom_path: Option<String>) -> Result<Config> {
    let config_path = get_config_path(custom_path);
    
    if config_path.exists() {
        info!("Loading config from: {}", config_path.display());
        let config_str = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;
            
        let config: Config = toml::from_str(&config_str)
            .with_context(|| "Failed to parse config file")?;
            
        Ok(config)
    } else {
        info!("Config file not found, creating default at: {}", config_path.display());
        let config = default_config();
        save_config(&config, &config_path)?;
        Ok(config)
    }
}

/// Save configuration to file
pub fn save_config(config: &Config, path: &Path) -> Result<()> {
    let config_str = toml::to_string(config)
        .with_context(|| "Failed to serialize configuration")?;
        
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }
    
    fs::write(path, config_str)
        .with_context(|| format!("Failed to write config to: {}", path.display()))?;
        
    Ok(())
}

/// Create default configuration
pub fn default_config() -> Config {
    Config {
        api_key: String::new(),
        temp_dir: get_temp_dir(),
        recording: RecordingConfig {
            max_duration_secs: 300,
            sample_rate: 44100,
            play_sounds: true,
        },
        ui: UiConfig {
            dark_mode: true,
            notification_enabled: true,
        },
        shortcuts: ShortcutConfig {
            toggle_recording: String::from("Shift+space"),
            clear_transcript: String::from("Alt+Shift+C"),
            copy_to_clipboard: String::from("Alt+Shift+X"),
            auto_paste: true,
        },
    }
} 