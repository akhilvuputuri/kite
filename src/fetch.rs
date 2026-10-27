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
            .user_agent(concat!("lockstep/", env!("CARGO_PKG_VERSION")))
            .proxy(ureq::Proxy::try_from_env())
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

/// GETs `url` and returns the body, retrying network failures, 429 and 5xx
/// the way npm's fetch does (two retries with backoff).
pub fn get(url: &str, accept: Option<&str>) -> Result<Vec<u8>, HttpError> {
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(500 * 4u64.pow(attempt - 1)));
        }
        match get_once(url, accept) {
            Ok(body) => return Ok(body),
            Err(error) => {
                let retry = match error.status {
                    None => true,
                    Some(status) => status == 408 || status == 429 || status >= 500,
                };
                if !retry {
                    return Err(error);
                }
                last = Some(error);
            }
        }
    }
    Err(last.expect("at least one attempt"))
}

fn get_once(url: &str, accept: Option<&str>) -> Result<Vec<u8>, HttpError> {
    let mut request = agent().get(url);
    if let Some(accept) = accept {
        request = request.header("accept", accept);
    }
    let response = request.call().map_err(|e| HttpError {
        message: format!("GET {url}: {e}"),
        status: None,
    })?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(HttpError {
            message: format!("GET {url}: HTTP {status}"),
            status: Some(status),
        });
    }
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut body)
        .map_err(|e| HttpError {
            message: format!("GET {url}: reading body: {e}"),
            status: None,
        })?;
    Ok(body)
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
