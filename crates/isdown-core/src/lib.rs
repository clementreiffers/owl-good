//! Shared contracts for monitoring and native presentation.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status { Healthy, Warning, Failed, #[default] Unknown }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules { pub fail: String, pub warning: String, pub success: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub interval_secs: Option<u64>,
    pub adapter: String,
    pub rules: Rules,
    pub icon_path: Option<String>,
    pub notifications: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub menu_title: String,
    pub show_menu_title: bool,
    pub show_site_icons: bool,
    pub icon_size: f64,
    pub font_size: f64,
    pub row_height: f64,
    pub panel_width: f64,
    pub healthy_color: String,
    pub warning_color: String,
    pub failed_color: String,
    pub unknown_color: String,
    pub monochrome_icons: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    pub enabled: bool,
    pub on_warning: bool,
    pub on_recovery: bool,
    pub sound: bool,
    pub confirmation_count: u32,
    pub cooldown_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub interval_secs: u64,
    pub timeout_secs: u64,
    pub stale_after_secs: u64,
    pub max_concurrent: usize,
    pub paused: bool,
    pub appearance: Appearance,
    pub notifications: Notifications,
    pub sites: Vec<Site>,
}

#[derive(Debug, Clone)]
pub struct Observation {
    pub site_id: String,
    pub status: Status,
    pub summary: String,
    pub checked_at: u64,
}

// Defaults and validation are supplied by the engine feature.
