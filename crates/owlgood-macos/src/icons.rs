//! A single background worker fetches bounded favicon files; AppKit never performs HTTP.
use std::{collections::HashSet, sync::mpsc, time::Duration};

pub(crate) struct Icons {
    pub requests: mpsc::Sender<String>,
    pub results: mpsc::Receiver<(String, Vec<u8>)>,
}

pub(crate) fn favicon_url(site_url: &str) -> Option<String> {
    let mut url = url::Url::parse(site_url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_path("/favicon.ico");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

pub(crate) fn start() -> Icons {
    let (requests, incoming) = mpsc::channel::<String>();
    let (outgoing, results) = mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = HashSet::new();
        while let Ok(url) = incoming.recv() {
            if seen.len() > 512 {
                seen.clear();
            }
            if !seen.insert(url.clone()) {
                continue;
            }
            if let Some(bytes) = fetch(&url)
                && outgoing.send((url, bytes)).is_err()
            {
                break;
            }
        }
    });
    Icons { requests, results }
}
fn fetch(url: &str) -> Option<Vec<u8>> {
    let mut easy = curl::easy::Easy::new();
    easy.url(url).ok()?;
    easy.timeout(Duration::from_secs(5)).ok()?;
    easy.connect_timeout(Duration::from_secs(3)).ok()?;
    // Redirects stay disabled: a favicon is only requested from the configured origin.
    let mut bytes = Vec::new();
    {
        let mut transfer = easy.transfer();
        transfer
            .write_function(|chunk| {
                if bytes.len() + chunk.len() > 128 * 1024 {
                    return Ok(0);
                }
                bytes.extend_from_slice(chunk);
                Ok(chunk.len())
            })
            .ok()?;
        transfer.perform().ok()?;
    }
    (easy.response_code().ok()? == 200 && !bytes.is_empty()).then_some(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_only() {
        assert_eq!(
            favicon_url("https://status.example.com/a?b#c").as_deref(),
            Some("https://status.example.com/favicon.ico")
        );
    }
    #[test]
    fn rejects_non_http() {
        assert!(favicon_url("file:fixture.png").is_none());
    }
}
