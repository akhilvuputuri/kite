//! HTTP access to the npm registry.

use std::io::Read;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use ureq::Agent;

pub const REGISTRY: &str = "https://registry.npmjs.org/";
const MAX_BODY: u64 = 1 << 30;
const ATTEMPTS: u32 = 3;

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .user_agent(concat!("kite/", env!("CARGO_PKG_VERSION")))
            .proxy(ureq::Proxy::try_from_env())
            // Keep one warm connection per download worker.
            .max_idle_connections(256)
            .max_idle_connections_per_host(128)
            .build()
            .into()
    })
}

#[derive(Debug)]
pub struct HttpError {
    pub message: String,
    pub status: Option<u16>,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub enum Revalidated {
    NotModified,
    Fresh { body: Vec<u8>, etag: Option<String> },
}

/// GETs `url`, sending `If-None-Match` when an ETag is known, with the same
/// retries as `get`.
pub fn get_revalidate(url: &str, accept: &str, etag: Option<&str>) -> Result<Revalidated, HttpError> {
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(500 * 4u64.pow(attempt - 1)));
        }
        let mut request = agent().get(url).header("accept", accept);
        if let Some(tag) = etag {
            request = request.header("if-none-match", tag);
        }
        let response = match request.call() {
            Ok(r) => r,
            Err(e) => {
                last = Some(HttpError {
                    message: format!("GET {url}: {e}"),
                    status: None,
                });
                continue;
            }
        };
        let status = response.status().as_u16();
        if status == 304 {
            return Ok(Revalidated::NotModified);
        }
        if status != 200 {
            let error = HttpError {
                message: format!("GET {url}: HTTP {status}"),
                status: Some(status),
            };
            if status == 408 || status == 429 || status >= 500 {
                last = Some(error);
                continue;
            }
            return Err(error);
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mut body = Vec::new();
        match response.into_body().into_reader().take(MAX_BODY).read_to_end(&mut body) {
            Ok(_) => return Ok(Revalidated::Fresh { body, etag }),
            Err(e) => {
                last = Some(HttpError {
                    message: format!("GET {url}: reading body: {e}"),
                    status: None,
                })
            }
        }
    }
    Err(last.expect("at least one attempt"))
}

/// Opens `url` and returns its body as a stream once the response is 200,
/// retrying connection failures, 429 and 5xx like `get`.
pub fn open(url: &str) -> Result<Box<dyn Read + Send>, HttpError> {
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(500 * 4u64.pow(attempt - 1)));
        }
        let response = match agent().get(url).call() {
            Ok(r) => r,
            Err(e) => {
                last = Some(HttpError {
                    message: format!("GET {url}: {e}"),
                    status: None,
                });
                continue;
            }
        };
        let status = response.status().as_u16();
        if status == 200 {
            return Ok(Box::new(response.into_body().into_reader().take(MAX_BODY)));
        }
        let error = HttpError {
            message: format!("GET {url}: HTTP {status}"),
            status: Some(status),
        };
        if !(status == 408 || status == 429 || status >= 500) {
            return Err(error);
        }
        last = Some(error);
    }
    Err(last.expect("at least one attempt"))
}

/// Only tarballs from the public npm registry are supported.
pub fn check_registry_url(url: &str) -> Result<(), String> {
    if url.starts_with(REGISTRY) && !url[REGISTRY.len()..].contains("..") {
        Ok(())
    } else {
        Err(format!(
            "unsupported package source {url:?}: only {REGISTRY} tarballs are supported"
        ))
    }
}
