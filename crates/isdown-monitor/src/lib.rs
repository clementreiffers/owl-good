//! Bounded background polling. AppKit never waits on network I/O.
use isdown_core::{CompiledRules, Config, Observation, Site, Status};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub enum Command {
    Refresh,
    UpdateConfig(Config),
    Shutdown,
}
pub struct Monitor {
    pub commands: Sender<Command>,
    pub observations: Receiver<Observation>,
}
const MAX_BODY: usize = 2 * 1024 * 1024;
const GITLAB_API: &str = "https://status.gitlab.com/1.0/status/5b36dc6502d06804c08349f7";
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Starts one scheduler and at most `max_concurrent` blocking libcurl workers.
/// Invalid updates are ignored; callers should validate before sending them.
pub fn start(config: Config) -> Monitor {
    let (commands, rx) = mpsc::channel();
    let (tx, observations) = mpsc::channel();
    thread::spawn(move || run(config, rx, tx));
    Monitor {
        commands,
        observations,
    }
}
struct Completion {
    generation: u64,
    observation: Observation,
}
fn rules(config: &Config) -> HashMap<String, Arc<CompiledRules>> {
    config
        .sites
        .iter()
        .filter_map(|s| {
            CompiledRules::new(&s.rules)
                .ok()
                .map(|r| (s.id.clone(), Arc::new(r)))
        })
        .collect()
}
fn run(mut config: Config, commands: Receiver<Command>, observations: Sender<Observation>) {
    if isdown_core::validate_config(&config).is_err() {
        return;
    }
    let (done_tx, done_rx) = mpsc::channel::<Completion>();
    let mut compiled = rules(&config);
    let mut generation = 0u64;
    let mut active = HashSet::new();
    let mut due: HashMap<String, Instant> = HashMap::new();
    let mut errors: HashMap<String, u32> = HashMap::new();
    loop {
        // Process commands before completions so removed sites cannot reappear.
        loop {
            match commands.try_recv() {
                Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return,
                Ok(Command::Refresh) => due.clear(),
                Ok(Command::UpdateConfig(next)) => {
                    if isdown_core::validate_config(&next).is_ok() {
                        config = next;
                        compiled = rules(&config);
                        generation = generation.wrapping_add(1);
                        due.clear();
                        errors.clear();
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        while let Ok(done) = done_rx.try_recv() {
            active.remove(&done.observation.site_id);
            if done.generation != generation {
                continue;
            }
            let id = done.observation.site_id.clone();
            let failures = errors.entry(id.clone()).or_default();
            if done.observation.status == Status::Unknown {
                *failures = failures.saturating_add(1);
            } else {
                *failures = 0;
            }
            let interval = config
                .sites
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.interval_secs)
                .unwrap_or(config.interval_secs);
            let backoff = 1u64 << (*failures).min(4);
            due.insert(
                id,
                Instant::now() + Duration::from_secs(interval.saturating_mul(backoff).min(86400)),
            );
            if observations.send(done.observation).is_err() {
                return;
            }
        }
        if !config.paused {
            for site in config.sites.iter().filter(|s| s.enabled) {
                if active.len() >= config.max_concurrent {
                    break;
                }
                if active.contains(&site.id)
                    || due.get(&site.id).is_some_and(|d| *d > Instant::now())
                {
                    continue;
                }
                let Some(rule) = compiled.get(&site.id).cloned() else {
                    continue;
                };
                active.insert(site.id.clone());
                let site = site.clone();
                let tx = done_tx.clone();
                let timeout = config.timeout_secs;
                thread::spawn(move || {
                    let observation = check(&site, &rule, timeout);
                    let _ = tx.send(Completion {
                        generation,
                        observation,
                    });
                });
            }
        }
        // A short bounded sleep keeps command handling responsive without a busy loop.
        thread::sleep(Duration::from_millis(100));
    }
}

fn fetch(url: &str, timeout: u64) -> Result<String, String> {
    let mut request = curl::easy::Easy::new();
    request.url(url).map_err(|e| e.to_string())?;
    request
        .timeout(Duration::from_secs(timeout))
        .map_err(|e| e.to_string())?;
    request
        .connect_timeout(Duration::from_secs(timeout.min(10)))
        .map_err(|e| e.to_string())?;
    request.follow_location(true).map_err(|e| e.to_string())?;
    request.max_redirections(5).map_err(|e| e.to_string())?;
    request
        .useragent("isDown/0.1 (+macOS status monitor)")
        .map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    let mut too_large = false;
    {
        let mut transfer = request.transfer();
        transfer
            .write_function(|bytes| {
                if body.len().saturating_add(bytes.len()) > MAX_BODY {
                    too_large = true;
                    return Ok(0);
                }
                body.extend_from_slice(bytes);
                Ok(bytes.len())
            })
            .map_err(|e| e.to_string())?;
        let result = transfer.perform();
        drop(transfer);
        if too_large {
            return Err("Response exceeds 2 MiB".into());
        }
        result.map_err(|e| format!("Network/TLS error: {e}"))?;
    }
    let code = request.response_code().map_err(|e| e.to_string())?;
    if !(200..300).contains(&code) {
        return Err(format!("Status page HTTP {code} (service state unknown)"));
    }
    String::from_utf8(body).map_err(|_| "Response is not UTF-8".into())
}
fn check(site: &Site, rules: &CompiledRules, timeout: u64) -> Observation {
    let url = if site.adapter == "gitlab" {
        GITLAB_API
    } else {
        &site.url
    };
    let result = fetch(url, timeout).and_then(|body| classify(&site.adapter, &body, rules));
    let (status, summary) = result.unwrap_or_else(|error| (Status::Unknown, error));
    Observation {
        site_id: site.id.clone(),
        status,
        summary,
        checked_at: now(),
    }
}
/// Interpret only current Status.io fields, never incident history or progression codes.
pub fn classify(
    adapter: &str,
    body: &str,
    rules: &CompiledRules,
) -> Result<(Status, String), String> {
    if adapter == "regex" {
        let status = rules.classify(body);
        return Ok((
            status,
            match status {
                Status::Failed => "Failure rule matched",
                Status::Warning => "Warning rule matched",
                Status::Healthy => "Success rule matched",
                Status::Unknown => "No rule matched",
            }
            .into(),
        ));
    }
    if !matches!(adapter, "gitlab" | "statusio") {
        return Err("Unsupported adapter".into());
    }
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("Invalid status JSON: {e}"))?;
    let result = json.get("result").ok_or("Missing result")?;
    let overall = result
        .get("status_overall")
        .ok_or("Missing current overall state")?;
    let mut entries = vec![overall];
    if let Some(components) = result.get("status").and_then(|s| s.as_array()) {
        for component in components {
            entries.push(component);
            if let Some(containers) = component.get("containers").and_then(|v| v.as_array()) {
                entries.extend(containers);
            }
        }
    }
    let mut statuses = Vec::new();
    let mut labels = Vec::new();
    for entry in entries {
        let label = entry
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or("Missing current status label")?;
        let code = entry
            .get("status_code")
            .and_then(|v| v.as_u64())
            .ok_or("Missing current status code")?;
        let status = if [100, 200, 300, 400, 500, 600].contains(&code) {
            rules.classify(label)
        } else {
            Status::Unknown
        };
        statuses.push(Observation {
            site_id: String::new(),
            status,
            summary: String::new(),
            checked_at: 0,
        });
        if !labels.iter().any(|s| s == label) {
            labels.push(label.to_string());
        }
    }
    Ok((isdown_core::aggregate(&statuses, 0, 0), labels.join(" · ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    fn rules() -> CompiledRules {
        CompiledRules::new(&isdown_core::Rules::default()).unwrap()
    }
    fn payload(code: u64, label: &str) -> String {
        serde_json::json!({"result":{"status_overall":{"status":label,"status_code":code},"incidents":[{"status":"Service Disruption","status_code":500}]}}).to_string()
    }
    #[test]
    fn statusio_all_codes() {
        for (code, label, want) in [
            (100, "Operational", Status::Healthy),
            (200, "Maintenance", Status::Warning),
            (300, "Degraded Performance", Status::Warning),
            (400, "Partial Service Disruption", Status::Warning),
            (500, "Service Disruption", Status::Failed),
            (600, "Security Event", Status::Failed),
            (700, "Operational", Status::Unknown),
        ] {
            assert_eq!(
                classify("gitlab", &payload(code, label), &rules())
                    .unwrap()
                    .0,
                want
            );
        }
    }
    #[test]
    fn actual_gitlab_fixture() {
        let body = include_str!("../tests/fixtures/gitlab-2026-10-15.json");
        assert_eq!(
            classify("gitlab", body, &rules()).unwrap().0,
            Status::Failed
        );
    }
    #[test]
    fn malformed_and_unknown() {
        assert!(classify("gitlab", "{}", &rules()).is_err());
        assert!(classify("gitlab", "<html>", &rules()).is_err());
        assert_eq!(
            classify("gitlab", &payload(100, "Novel state"), &rules())
                .unwrap()
                .0,
            Status::Unknown
        );
    }
    fn server(response: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            let _ = socket.read(&mut request);
            let _ = socket.write_all(response.as_bytes());
        });
        url
    }
    #[test]
    fn transport_success_http_error_and_size_bound() {
        let url = server("HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nOperational".into());
        assert_eq!(fetch(&url, 2).unwrap(), "Operational");
        let url = server("HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n".into());
        assert!(fetch(&url, 2).unwrap_err().contains("429"));
        let content = "x".repeat(MAX_BODY + 1);
        let url = server(format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            content.len(),
            content
        ));
        assert!(fetch(&url, 2).unwrap_err().contains("2 MiB"));
    }
    #[test]
    fn worker_observation_and_shutdown() {
        let url = server("HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nOperational".into());
        let mut config = Config::default();
        config.sites[0].url = url;
        config.sites[0].adapter = "regex".into();
        let monitor = start(config);
        let obs = monitor
            .observations
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        assert_eq!(obs.status, Status::Healthy);
        monitor.commands.send(Command::Shutdown).unwrap();
        assert!(
            monitor
                .observations
                .recv_timeout(Duration::from_secs(1))
                .is_err()
        );
    }
    #[test]
    fn paused_does_not_poll() {
        let config = Config {
            paused: true,
            ..Config::default()
        };
        let monitor = start(config);
        assert!(
            monitor
                .observations
                .recv_timeout(Duration::from_millis(250))
                .is_err()
        );
        monitor.commands.send(Command::Shutdown).unwrap();
    }
}
