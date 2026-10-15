//! Bounded background polling. AppKit never waits on network I/O.
use owlgood_core::{CompiledRules, Config, Observation, Site, Status};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// Configuration changes are rare; keeping the public channel API owned avoids a second allocation.
#[allow(clippy::large_enum_variant)]
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
    retry_after: Option<u64>,
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
    if owlgood_core::validate_config(&config).is_err() {
        return;
    }
    let (done_tx, done_rx) = mpsc::channel::<Completion>();
    let mut compiled = rules(&config);
    let mut generation = 0u64;
    let mut active: HashMap<String, Arc<AtomicBool>> = HashMap::new();
    let mut refresh_pending = HashSet::new();
    let mut due: HashMap<String, Instant> = HashMap::new();
    let mut errors: HashMap<String, u32> = HashMap::new();
    loop {
        // Process commands before completions so removed sites cannot reappear.
        loop {
            match commands.try_recv() {
                Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                    for cancel in active.values() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                    return;
                }
                Ok(Command::Refresh) => {
                    due.clear();
                    refresh_pending.extend(active.keys().cloned());
                }
                Ok(Command::UpdateConfig(next)) => {
                    if owlgood_core::validate_config(&next).is_ok() {
                        for cancel in active.values() {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        refresh_pending.clear();
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
            let delay =
                retry_delay(interval, *failures).max(done.retry_after.unwrap_or(0).min(86400));
            due.insert(id.clone(), Instant::now() + Duration::from_secs(delay));
            if refresh_pending.remove(&id) {
                due.remove(&id);
            }
            if observations.send(done.observation).is_err() {
                for cancel in active.values() {
                    cancel.store(true, Ordering::Relaxed);
                }
                return;
            }
        }
        if !config.paused {
            for site in config.sites.iter().filter(|s| s.enabled) {
                if active.len() >= config.max_concurrent {
                    break;
                }
                if active.contains_key(&site.id)
                    || due.get(&site.id).is_some_and(|d| *d > Instant::now())
                {
                    continue;
                }
                let Some(rule) = compiled.get(&site.id).cloned() else {
                    continue;
                };
                let cancel = Arc::new(AtomicBool::new(false));
                active.insert(site.id.clone(), cancel.clone());
                let site = site.clone();
                let tx = done_tx.clone();
                let timeout = config.timeout_secs;
                thread::spawn(move || {
                    let (observation, retry_after) = check(&site, &rule, timeout, &cancel);
                    let _ = tx.send(Completion {
                        generation,
                        observation,
                        retry_after,
                    });
                });
            }
        }
        // A short bounded sleep keeps command handling responsive without a busy loop.
        thread::sleep(Duration::from_millis(100));
    }
}

fn retry_delay(interval: u64, failures: u32) -> u64 {
    interval.saturating_mul(1u64 << failures.min(4)).min(86400)
}

#[derive(Debug)]
struct FetchFailure {
    message: String,
    retry_after: Option<u64>,
}
impl From<String> for FetchFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            retry_after: None,
        }
    }
}
impl From<&str> for FetchFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

#[cfg(test)]
fn fetch(url: &str, timeout: u64) -> Result<String, String> {
    fetch_cancellable(url, timeout, &AtomicBool::new(false)).map_err(|e| e.message)
}
fn fetch_cancellable(url: &str, timeout: u64, cancel: &AtomicBool) -> Result<String, FetchFailure> {
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
        .useragent("OwlGood/0.1 (+macOS status monitor)")
        .map_err(|e| e.to_string())?;
    request.progress(true).map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    let mut too_large = false;
    let mut retry_after = None;
    {
        let mut transfer = request.transfer();
        transfer
            .header_function(|header| {
                if let Ok(line) = std::str::from_utf8(header) {
                    if line.starts_with("HTTP/") {
                        retry_after = None;
                    }
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("retry-after")
                    {
                        retry_after = value.trim().parse::<u64>().ok().or_else(|| {
                            httpdate::parse_http_date(value.trim())
                                .ok()
                                .and_then(|date| date.duration_since(SystemTime::now()).ok())
                                .map(|d| d.as_secs())
                        });
                    }
                }
                true
            })
            .map_err(|e| e.to_string())?;
        transfer
            .progress_function(|_, _, _, _| !cancel.load(Ordering::Relaxed))
            .map_err(|e| e.to_string())?;
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
        return Err(FetchFailure {
            message: format!("Status page HTTP {code} (service state unknown)"),
            retry_after: if code == 429 || code == 503 {
                retry_after
            } else {
                None
            },
        });
    }
    String::from_utf8(body).map_err(|_| "Response is not UTF-8".into())
}
fn check(
    site: &Site,
    rules: &CompiledRules,
    timeout: u64,
    cancel: &AtomicBool,
) -> (Observation, Option<u64>) {
    let url = if site.adapter == "gitlab" {
        GITLAB_API
    } else {
        &site.url
    };
    let result = fetch_cancellable(url, timeout, cancel)
        .and_then(|body| classify(&site.adapter, &body, rules).map_err(FetchFailure::from));
    let (status, summary, retry_after) = match result {
        Ok((status, summary)) => (status, summary, None),
        Err(error) => (Status::Unknown, error.message, error.retry_after),
    };
    (
        Observation {
            site_id: site.id.clone(),
            status,
            summary,
            checked_at: now(),
        },
        retry_after,
    )
}
// Validate provider schema before applying user rules. A contradictory code/label
// cannot silently report healthy, nor can a future provider value masquerade as known.
fn known_label(code: u64, label: &str) -> bool {
    let aliases: &[&str] = match code {
        100 => &["Operational"],
        200 => &["Maintenance", "Under Maintenance", "Planned Maintenance"],
        300 => &["Degraded Performance", "Degraded"],
        400 => &["Partial Service Disruption", "Partial outage"],
        500 => &["Service Disruption", "Major outage"],
        600 => &["Security Event", "Critical"],
        _ => &[],
    };
    aliases
        .iter()
        .any(|alias| label.eq_ignore_ascii_case(alias))
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
        let status = if known_label(code, label) {
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
    Ok((owlgood_core::aggregate(&statuses, 0, 0), labels.join(" · ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    fn rules() -> CompiledRules {
        CompiledRules::new(&owlgood_core::Rules::default()).unwrap()
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
    #[test]
    fn retry_cadence_is_bounded() {
        assert_eq!(retry_delay(30, 0), 30);
        assert_eq!(retry_delay(5, 2), 20);
        assert_eq!(retry_delay(30, 100), 480);
        assert_eq!(retry_delay(86400, 2), 86400);
    }
    #[test]
    fn cancelling_request_aborts() {
        let url = server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into());
        assert!(fetch_cancellable(&url, 2, &AtomicBool::new(true)).is_err());
    }
    #[test]
    fn removed_site_in_flight_never_reappears() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = [0; 2048];
            let _ = socket.read(&mut buf);
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nOperational");
        });
        let mut config = Config::default();
        config.sites[0].adapter = "regex".into();
        config.sites[0].url = url;
        let monitor = start(config.clone());
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        config.sites.clear();
        monitor
            .commands
            .send(Command::UpdateConfig(config))
            .unwrap();
        // Commands are consumed before completion, even when response is released immediately.
        release_tx.send(()).unwrap();
        assert!(
            monitor
                .observations
                .recv_timeout(Duration::from_millis(400))
                .is_err()
        );
        monitor.commands.send(Command::Shutdown).unwrap();
    }
    #[test]
    fn custom_regex_and_historical_events() {
        let custom = CompiledRules::new(&owlgood_core::Rules {
            fail: "^Operational$".into(),
            warning: String::new(),
            success: String::new(),
        })
        .unwrap();
        assert_eq!(
            classify("gitlab", &payload(100, "Operational"), &custom)
                .unwrap()
                .0,
            Status::Failed
        );
        assert_eq!(
            classify("gitlab", &payload(100, "Operational"), &rules())
                .unwrap()
                .0,
            Status::Healthy
        );
        for (code, label, want) in [
            (300, "Degraded", Status::Warning),
            (400, "Partial outage", Status::Warning),
            (500, "Major outage", Status::Failed),
            (600, "Critical", Status::Failed),
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
    #[ignore = "Opt-in live contract verification: requires Internet"]
    fn live_gitlab_contract() {
        let result = fetch(GITLAB_API, 10).unwrap();
        let (_, summary) = classify("gitlab", &result, &rules()).unwrap();
        assert!(!summary.is_empty());
    }
    #[test]
    fn retry_after_preserved_for_scheduler() {
        let url = server(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 120\r\nContent-Length: 0\r\n\r\n"
                .into(),
        );
        let error = fetch_cancellable(&url, 2, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.retry_after, Some(120));
    }
    #[test]
    fn contradictory_code_label_is_unknown() {
        for (code, label) in [
            (500, "Operational"),
            (100, "Service Disruption"),
            (300, "Maintenance"),
            (200, "Investigating"),
        ] {
            assert_eq!(
                classify("gitlab", &payload(code, label), &rules())
                    .unwrap()
                    .0,
                Status::Unknown
            );
        }
    }
    #[test]
    fn concurrency_bound_and_refresh_do_not_overlap_site() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (accepted_tx, accepted_rx) = mpsc::channel();
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let (release_tx, release_rx) = mpsc::channel();
                accepted_tx.send(release_tx).unwrap();
                thread::spawn(move || {
                    let mut request = [0; 2048];
                    let _ = socket.read(&mut request);
                    release_rx.recv().unwrap();
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nOperational");
                });
            }
        });
        let mut config = Config {
            max_concurrent: 1,
            ..Config::default()
        };
        config.sites[0].url = url;
        config.sites[0].adapter = "regex".into();
        let monitor = start(config);
        let release_first = accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        monitor.commands.send(Command::Refresh).unwrap();
        assert!(
            accepted_rx
                .recv_timeout(Duration::from_millis(250))
                .is_err(),
            "refresh overlapped the active request"
        );
        release_first.send(()).unwrap();
        let release_second = accepted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("manual refresh must run after completion");
        release_second.send(()).unwrap();
        monitor
            .observations
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        monitor
            .observations
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        monitor.commands.send(Command::Shutdown).unwrap();
    }
    /// Local repeatable measurement, intentionally not a machine-specific performance gate.
    #[test]
    #[ignore = "Opt-in local scheduler measurements for 1/10/50 services"]
    fn local_scheduler_measurements() {
        use std::sync::atomic::AtomicUsize;
        for count in [1usize, 10, 50] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let stopped = Arc::new(AtomicBool::new(false));
            let active = Arc::new(AtomicUsize::new(0));
            let maximum = Arc::new(AtomicUsize::new(0));
            let total = Arc::new(AtomicUsize::new(0));
            let server_stop = stopped.clone();
            let server_active = active.clone();
            let server_max = maximum.clone();
            let server_total = total.clone();
            let server = thread::spawn(move || {
                let mut handlers = Vec::new();
                while !server_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            server_total.fetch_add(1, Ordering::Relaxed);
                            let active = server_active.clone();
                            let maximum = server_max.clone();
                            handlers.push(thread::spawn(move || {
                                let current = active.fetch_add(1, Ordering::Relaxed) + 1;
                                maximum.fetch_max(current, Ordering::Relaxed);
                                let mut request = [0; 2048];
                                socket
                                    .set_read_timeout(Some(Duration::from_secs(2)))
                                    .unwrap();
                                let _ = socket.read(&mut request);
                                thread::sleep(Duration::from_millis(20));
                                let _ = socket.write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nOperational",
                                );
                                active.fetch_sub(1, Ordering::Relaxed);
                            }));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(e) => panic!("local server: {e}"),
                    }
                }
                for handler in handlers {
                    handler.join().unwrap();
                }
            });
            let config = Config {
                max_concurrent: 4,
                sites: (0..count)
                    .map(|i| Site {
                        id: format!("local-{i}"),
                        url: url.clone(),
                        adapter: "regex".into(),
                        ..Site::default()
                    })
                    .collect(),
                ..Config::default()
            };
            let started = Instant::now();
            let monitor = start(config);
            let mut ids = HashSet::new();
            for _ in 0..count {
                let observation = monitor
                    .observations
                    .recv_timeout(Duration::from_secs(30))
                    .unwrap();
                assert_eq!(observation.status, Status::Healthy);
                assert!(ids.insert(observation.site_id));
            }
            let elapsed = started.elapsed();
            monitor.commands.send(Command::Shutdown).unwrap();
            assert!(matches!(
                monitor.observations.recv_timeout(Duration::from_secs(2)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ));
            let before = total.load(Ordering::Relaxed);
            thread::sleep(Duration::from_millis(150));
            let after = total.load(Ordering::Relaxed);
            stopped.store(true, Ordering::Relaxed);
            server.join().unwrap();
            assert_eq!(after, before);
            assert_eq!(after, count);
            assert_eq!(active.load(Ordering::Relaxed), 0);
            assert!(maximum.load(Ordering::Relaxed) <= 4);
            eprintln!(
                "sites={count} collection_ms={} peak_requests={} new_requests_after_shutdown={} active_after_shutdown=0",
                elapsed.as_millis(),
                maximum.load(Ordering::Relaxed),
                after - before
            );
        }
    }
    #[test]
    fn shutdown_cancels_inflight_network_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (started_tx, started_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(6)))
                .unwrap();
            let mut request = [0; 2048];
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
                assert!(header.len() < 8192);
            }
            started_tx.send(()).unwrap();
            closed_tx
                .send(matches!(socket.read(&mut request), Ok(0)))
                .unwrap();
        });
        let mut config = Config {
            timeout_secs: 10,
            ..Config::default()
        };
        config.sites[0].adapter = "regex".into();
        config.sites[0].url = url;
        let monitor = start(config);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        monitor.commands.send(Command::Shutdown).unwrap();
        assert!(
            closed_rx.recv_timeout(Duration::from_secs(7)).unwrap(),
            "shutdown must close the HTTP connection before its configured timeout"
        );
        assert!(matches!(
            monitor.observations.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
