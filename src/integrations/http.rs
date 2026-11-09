//! The HTTP client every integration shares: one request at a time, paced to
//! the service's rate limit, retrying rate limits and server errors.

use std::cell::Cell;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use log::{debug, warn};

/// Give up instead of sleeping when a service asks for a longer wait than
/// this; a multi-hour Retry-After means a quota, not a blip.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
const MAX_ATTEMPTS: u32 = 5;

/// A response with any status. Rate limits and server errors have already
/// been retried; whatever is left is for the caller to interpret.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub struct Http {
    agent: ureq::Agent,
    service: &'static str,
    min_interval: Duration,
    last_sent: Cell<Option<Instant>>,
}

impl Http {
    /// `service` names the integration in logs and errors.
    pub fn new(service: &'static str, user_agent: &str, requests_per_second: f64) -> Http {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(user_agent)
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Http {
            agent: ureq::Agent::new_with_config(config),
            service,
            min_interval: Duration::from_secs_f64(1.0 / requests_per_second),
            last_sent: Cell::new(None),
        }
    }

    pub fn get(&self, url: &str, query: &[(&str, &str)], bearer: Option<&str>) -> Result<Response> {
        self.send_with_retries(url, || {
            let mut request = self.agent.get(url).query_pairs(query.iter().copied());
            if let Some(token) = bearer {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            request.call()
        })
    }

    pub fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<Response> {
        self.send_with_retries(url, || self.agent.post(url).send_form(form.iter().copied()))
    }

    /// Sends `json` with `method` ("POST" or "PUT").
    pub fn send_json(
        &self,
        method: &str,
        url: &str,
        json: &serde_json::Value,
        bearer: &str,
    ) -> Result<Response> {
        self.send_with_retries(url, || {
            let request = match method {
                "PUT" => self.agent.put(url),
                _ => self.agent.post(url),
            };
            request
                .header("Authorization", format!("Bearer {bearer}"))
                .send_json(json)
        })
    }

    fn send_with_retries(
        &self,
        url: &str,
        mut send: impl FnMut() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<Response> {
        let mut backoff = Duration::from_secs(1);
        for attempt in 1..=MAX_ATTEMPTS {
            self.pace();
            let last_attempt = attempt == MAX_ATTEMPTS;
            let mut response = match send() {
                Ok(response) => response,
                Err(e) if !last_attempt => {
                    warn!("{}: {e}; retrying in {backoff:?}", self.service);
                    sleep(backoff);
                    backoff *= 2;
                    continue;
                }
                Err(e) => bail!("{}: request to {url} failed: {e}", self.service),
            };
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string()?;
            debug!("{} {status} {url}", self.service);

            if status == 429 && !last_attempt {
                let wait = retry_after(&response).unwrap_or(backoff);
                if wait > MAX_RETRY_AFTER {
                    bail!("{}: rate limited for {wait:?}: {body}", self.service);
                }
                warn!("{}: rate limited; waiting {wait:?}", self.service);
                sleep(wait);
                backoff *= 2;
                continue;
            }
            if status >= 500 && !last_attempt {
                warn!(
                    "{}: server error {status}; retrying in {backoff:?}",
                    self.service
                );
                sleep(backoff);
                backoff *= 2;
                continue;
            }
            return Ok(Response { status, body });
        }
        unreachable!("the last attempt always returns")
    }

    /// Sleeps until `min_interval` has passed since the previous request.
    fn pace(&self) {
        if let Some(last) = self.last_sent.get() {
            let elapsed = last.elapsed();
            if elapsed < self.min_interval {
                sleep(self.min_interval - elapsed);
            }
        }
        self.last_sent.set(Some(Instant::now()));
    }
}

fn retry_after(response: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    let seconds: u64 = response
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}
