//! Shared contracts for monitoring and native presentation.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Healthy,
    Warning,
    Failed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    pub fail: String,
    pub warning: String,
    pub success: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub theme: String,
    pub menu_title: String,
    pub menu_symbol: String,
    pub menu_icon_path: Option<String>,
    pub menu_icon_size: f64,
    pub badge_size: f64,
    pub show_badge: bool,
    pub badge_position: String,
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
#[serde(default, deny_unknown_fields)]
pub struct Notifications {
    pub enabled: bool,
    pub on_warning: bool,
    pub on_recovery: bool,
    pub sound: bool,
    pub confirmation_count: u32,
    pub cooldown_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

impl Default for Rules {
    fn default() -> Self {
        Self {
        fail: r"(?im)^(?:Service Disruption|Security Event|Major outage|Critical)$".into(),
        warning: r"(?im)^(?:Partial Service Disruption|Degraded Performance|Degraded|Partial outage|Maintenance|Under Maintenance|Planned Maintenance)$".into(),
        success: r"(?im)^Operational$".into(),
    }
    }
}
impl Default for Site {
    fn default() -> Self {
        Self {
            id: "gitlab".into(),
            name: "GitLab".into(),
            url: "https://status.gitlab.com/".into(),
            enabled: true,
            interval_secs: None,
            adapter: "gitlab".into(),
            rules: Rules::default(),
            icon_path: None,
            notifications: true,
        }
    }
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            menu_title: "OwlGood".into(),
            menu_symbol: "owlgood".into(),
            menu_icon_path: None,
            menu_icon_size: 18.,
            badge_size: 9.,
            show_badge: true,
            badge_position: "right".into(),
            show_menu_title: false,
            show_site_icons: true,
            icon_size: 28.,
            font_size: 13.,
            row_height: 44.,
            panel_width: 300.,
            healthy_color: "#34C759".into(),
            warning_color: "#FF9500".into(),
            failed_color: "#FF3B30".into(),
            unknown_color: "#8E8E93".into(),
            monochrome_icons: false,
        }
    }
}
impl Default for Notifications {
    fn default() -> Self {
        Self {
            enabled: true,
            on_warning: true,
            on_recovery: true,
            sound: true,
            confirmation_count: 1,
            cooldown_secs: 300,
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            interval_secs: 30,
            timeout_secs: 10,
            stale_after_secs: 120,
            max_concurrent: 4,
            paused: false,
            appearance: Appearance::default(),
            notifications: Notifications::default(),
            sites: vec![Site::default()],
        }
    }
}

/// Compiled once per configuration generation; an empty expression disables that rule.
pub struct CompiledRules {
    fail: Option<regex::Regex>,
    warning: Option<regex::Regex>,
    success: Option<regex::Regex>,
}
impl CompiledRules {
    pub fn new(rules: &Rules) -> Result<Self, String> {
        fn compile(s: &str) -> Result<Option<regex::Regex>, String> {
            if s.len() > 8192 {
                return Err("Regex exceeds 8192 bytes".into());
            }
            if s.is_empty() {
                return Ok(None);
            }
            regex::RegexBuilder::new(s)
                .size_limit(2 * 1024 * 1024)
                .build()
                .map(Some)
                .map_err(|e| e.to_string())
        }
        Ok(Self {
            fail: compile(&rules.fail)?,
            warning: compile(&rules.warning)?,
            success: compile(&rules.success)?,
        })
    }
    pub fn classify(&self, text: &str) -> Status {
        for (rule, status) in [
            (&self.fail, Status::Failed),
            (&self.warning, Status::Warning),
            (&self.success, Status::Healthy),
        ] {
            if rule.as_ref().is_some_and(|r| r.is_match(text)) {
                return status;
            }
        }
        Status::Unknown
    }
}

/// Maximum known severity, with uncertainty taking precedence over healthy.
pub fn aggregate(observations: &[Observation], now: u64, stale_after_secs: u64) -> Status {
    let mut result = Status::Healthy;
    if observations.is_empty() {
        return Status::Unknown;
    }
    for item in observations {
        let state =
            if item.checked_at > now || now.saturating_sub(item.checked_at) > stale_after_secs {
                Status::Unknown
            } else {
                item.status
            };
        result = match (result, state) {
            (Status::Failed, _) | (_, Status::Failed) => Status::Failed,
            (Status::Warning, _) | (_, Status::Warning) => Status::Warning,
            (Status::Unknown, _) | (_, Status::Unknown) => Status::Unknown,
            _ => Status::Healthy,
        };
    }
    result
}
pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("OWLGOOD_CONFIG_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Library/Application Support/OwlGood/config.json")
}
fn validate_icon_path(path: Option<&str>) -> Result<(), String> {
    if let Some(path) = path
        && (path.trim().is_empty() || path.len() > 4096 || path.contains('\0'))
    {
        return Err("Icon path must be nonempty, at most 4096 bytes, without NUL".into());
    }
    Ok(())
}
pub fn validate_config(c: &Config) -> Result<(), String> {
    if c.version != 1 {
        return Err("Unsupported configuration version (expected 1)".into());
    }
    if !(5..=86400).contains(&c.interval_secs)
        || !(1..=120).contains(&c.timeout_secs)
        || !(5..=604800).contains(&c.stale_after_secs)
        || !(1..=16).contains(&c.max_concurrent)
    {
        return Err("Invalid timing or concurrency: interval 5–86400s, timeout 1–120s, stale 5–604800s, concurrency 1–16".into());
    }
    if !(1..=100).contains(&c.notifications.confirmation_count)
        || c.notifications.cooldown_secs > 604800
    {
        return Err("Invalid notification confirmation/cooldown".into());
    }
    if c.sites.len() > 500 {
        return Err("Maximum 500 sites".into());
    }
    let mut ids = HashSet::new();
    for site in &c.sites {
        if site.id.trim().is_empty()
            || site.name.trim().is_empty()
            || site.id.len() > 128
            || site.name.len() > 256
            || !ids.insert(&site.id)
        {
            return Err("Sites require unique nonempty IDs and names".into());
        }
        let url = url::Url::parse(&site.url).map_err(|e| format!("{}: {e}", site.name))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(
                "Site URL must use HTTP(S), with a host and no embedded credentials".into(),
            );
        }
        if site.adapter == "gitlab"
            && (url.host_str() != Some("status.gitlab.com") || url.scheme() != "https")
        {
            return Err("GitLab adapter requires https://status.gitlab.com/; choose regex or statusio for other sites".into());
        }
        if !matches!(site.adapter.as_str(), "gitlab" | "statusio" | "regex") {
            return Err(format!("Unknown adapter: {}", site.adapter));
        }
        if site
            .interval_secs
            .is_some_and(|i| !(5..=86400).contains(&i))
        {
            return Err("Site interval must be 5–86400 seconds".into());
        }
        validate_icon_path(site.icon_path.as_deref())?;
        CompiledRules::new(&site.rules).map_err(|e| format!("{}: {e}", site.name))?;
    }
    let a = &c.appearance;
    validate_icon_path(a.menu_icon_path.as_deref())?;
    if !matches!(a.badge_position.as_str(), "left" | "right") {
        return Err("Badge position must be left or right".into());
    }
    if a.menu_symbol.trim().is_empty() || a.menu_symbol.len() > 128 {
        return Err("Menu symbol requires a nonempty SF Symbol name (max 128 bytes)".into());
    }
    if a.menu_title.len() > 128 {
        return Err("Menu title exceeds 128 bytes".into());
    }
    if !matches!(a.theme.as_str(), "system" | "light" | "dark") {
        return Err("Theme must be system, light or dark".into());
    }
    for (value, min, max) in [
        (a.icon_size, 12., 64.),
        (a.menu_icon_size, 12., 32.),
        (a.badge_size, 4., 16.),
        (a.font_size, 10., 32.),
        (a.row_height, 24., 120.),
        (a.panel_width, 260., 900.),
    ] {
        if !value.is_finite() || !(min..=max).contains(&value) {
            return Err("Appearance dimensions out of range".into());
        }
    }
    for color in [
        &a.healthy_color,
        &a.warning_color,
        &a.failed_color,
        &a.unknown_color,
    ] {
        if color.len() != 7
            || !color.starts_with('#')
            || !color[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Colors must use #RRGGBB".into());
        }
    }
    Ok(())
}
pub fn load_config(path: &Path) -> Result<Config, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("Configuration exceeds 2 MiB".into());
    }
    let c: Config = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate_config(&c)?;
    Ok(c)
}
/// Atomically replace a validated JSON file, never truncating a previous configuration.
pub fn save_config(path: &Path, c: &Config) -> Result<(), String> {
    validate_config(c)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut temp, c).map_err(|e| e.to_string())?;
    temp.write_all(b"\n")
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    temp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_valid() {
        validate_config(&Config::default()).unwrap();
    }
    #[test]
    fn regex_inventory() {
        let r = CompiledRules::new(&Rules::default()).unwrap();
        for (s, want) in [
            ("Operational", Status::Healthy),
            ("Maintenance", Status::Warning),
            ("Under Maintenance", Status::Warning),
            ("Planned Maintenance", Status::Warning),
            ("Degraded Performance", Status::Warning),
            ("Partial Service Disruption", Status::Warning),
            ("Service Disruption", Status::Failed),
            ("Security Event", Status::Failed),
            ("Investigating", Status::Unknown),
            ("future", Status::Unknown),
        ] {
            assert_eq!(r.classify(s), want, "{s}");
        }
    }
    #[test]
    fn regex_priority_and_disabled() {
        let r = CompiledRules::new(&Rules {
            fail: "fail".into(),
            warning: "warn".into(),
            success: "ok".into(),
        })
        .unwrap();
        assert_eq!(r.classify("ok warn fail"), Status::Failed);
        assert_eq!(r.classify("ok warn"), Status::Warning);
        assert_eq!(
            CompiledRules::new(&Rules {
                fail: String::new(),
                warning: String::new(),
                success: String::new()
            })
            .unwrap()
            .classify(""),
            Status::Unknown
        );
    }
    #[test]
    fn invalid_regex() {
        assert!(
            CompiledRules::new(&Rules {
                fail: "(?=x)".into(),
                ..Rules::default()
            })
            .is_err()
        );
    }
    #[test]
    fn aggregate_uncertainty() {
        let make = |s, t| Observation {
            site_id: "a".into(),
            status: s,
            summary: String::new(),
            checked_at: t,
        };
        assert_eq!(aggregate(&[], 100, 30), Status::Unknown);
        assert_eq!(
            aggregate(&[make(Status::Healthy, 60)], 100, 30),
            Status::Unknown
        );
        assert_eq!(
            aggregate(&[make(Status::Healthy, 101)], 100, 30),
            Status::Unknown
        );
        for a in [
            Status::Healthy,
            Status::Warning,
            Status::Failed,
            Status::Unknown,
        ] {
            for b in [
                Status::Healthy,
                Status::Warning,
                Status::Failed,
                Status::Unknown,
            ] {
                assert_eq!(
                    aggregate(&[make(a, 100), make(b, 100)], 100, 30),
                    aggregate(&[make(b, 100), make(a, 100)], 100, 30)
                );
            }
        }
    }
    #[test]
    fn persistence_removal_and_atomic_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(load_config(&path).unwrap().sites.len(), 1);
        let mut c = Config::default();
        c.sites.clear();
        save_config(&path, &c).unwrap();
        assert!(load_config(&path).unwrap().sites.is_empty());
        c.version = 999;
        assert!(save_config(&path, &c).is_err());
        assert!(load_config(&path).unwrap().sites.is_empty());
        fs::write(&path, "broken").unwrap();
        assert!(load_config(&path).is_err());
    }
    #[test]
    fn validation_guards() {
        let mut c = Config::default();
        c.sites.push(Site::default());
        assert!(validate_config(&c).is_err());
        c.sites.pop();
        c.sites[0].url = "file:fixture.json".into();
        assert!(validate_config(&c).is_err());
        c.sites[0] = Site::default();
        c.appearance.font_size = f64::NAN;
        assert!(validate_config(&c).is_err());
    }
    #[test]
    fn json_defaults_empty_list_and_typos() {
        let c: Config = serde_json::from_str(r#"{"sites":[]}"#).unwrap();
        assert_eq!(c.interval_secs, 30);
        assert!(c.sites.is_empty());
        validate_config(&c).unwrap();
        assert!(serde_json::from_str::<Config>(r#"{"interval_sec":60}"#).is_err());
        assert!(serde_json::from_str::<Config>(r#"{"appearance":{"them":"dark"}}"#).is_err());
        let c: Config = serde_json::from_str(r#"{"version":999}"#).unwrap();
        assert!(validate_config(&c).is_err());
    }
    #[test]
    fn url_and_timing_bounds() {
        for url in [
            "https://user:password@example.com/",
            "https://user@example.com/",
            "https://",
            "not a url",
            "file:fixture.json",
        ] {
            let mut c = Config::default();
            c.sites[0].adapter = "regex".into();
            c.sites[0].url = url.into();
            assert!(validate_config(&c).is_err(), "{url}");
        }
        let mut c = Config::default();
        c.sites[0].url = "https://example.com/".into();
        assert!(validate_config(&c).is_err());
        for interval in [0, 4, 86401, u64::MAX] {
            let c = Config {
                interval_secs: interval,
                ..Config::default()
            };
            assert!(validate_config(&c).is_err());
        }
        for timeout in [0, 121, u64::MAX] {
            let c = Config {
                timeout_secs: timeout,
                ..Config::default()
            };
            assert!(validate_config(&c).is_err());
        }
        for concurrent in [0, 17, usize::MAX] {
            let c = Config {
                max_concurrent: concurrent,
                ..Config::default()
            };
            assert!(validate_config(&c).is_err());
        }
        for interval in [5, 86400] {
            let c = Config {
                interval_secs: interval,
                ..Config::default()
            };
            validate_config(&c).unwrap();
        }
    }
    #[test]
    fn truncated_and_oversized_config_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, b"{\"version\":").unwrap();
        assert!(load_config(&path).is_err());
        fs::write(&path, vec![b' '; 2 * 1024 * 1024 + 1]).unwrap();
        assert!(load_config(&path).unwrap_err().contains("2 MiB"));
    }
    #[test]
    fn menu_appearance_validation_and_defaults() {
        let mut c = Config::default();
        assert_eq!(c.appearance.menu_symbol, "owlgood");
        assert!(c.appearance.show_badge);
        c.appearance.menu_icon_size = 33.;
        assert!(validate_config(&c).is_err());
        c.appearance.menu_icon_size = 18.;
        c.appearance.badge_size = 3.;
        assert!(validate_config(&c).is_err());
        c.appearance.badge_size = 9.;
        c.appearance.badge_position = "top".into();
        assert!(validate_config(&c).is_err());
        c.appearance.badge_position = "left".into();
        c.appearance.menu_icon_path = Some(String::new());
        assert!(validate_config(&c).is_err());
        c.appearance.menu_icon_path = None;
        validate_config(&c).unwrap();
        let appearance: Appearance = serde_json::from_str("{}").unwrap();
        assert_eq!(appearance.menu_icon_size, 18.);
    }
}
