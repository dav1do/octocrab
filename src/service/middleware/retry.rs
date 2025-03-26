use std::pin::Pin;

use chrono::{DateTime, Duration, Utc};
use futures_util::future;
use http::{HeaderMap, Request, Response, StatusCode};
use hyper_util::client::legacy::Error;
use tokio::time::sleep;
use tower::retry::Policy;

use crate::body::OctoBody;

#[derive(Clone)]
pub enum RetryConfig {
    None,
    Simple(usize),
}

impl<B> Policy<Request<OctoBody>, Response<B>, Error> for RetryConfig {
    type Future = Pin<Box<dyn futures_util::future::Future<Output = ()> + Send>>;

    fn retry(
        &mut self,
        _req: &mut Request<OctoBody>,
        result: &mut Result<Response<B>, Error>,
    ) -> Option<Self::Future> {
        match self {
            RetryConfig::None => None,
            RetryConfig::Simple(remaining_attempts) => {
                if *remaining_attempts == 0 {
                    return None;
                }
                *remaining_attempts -= 1;

                match result {
                    Ok(response) => match response.status() {
                        s if s.is_server_error() => Some(Box::pin(future::ready(()))),
                        StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN => {
                            let headers = response.headers();

                            if let Some(rate_limit_info) = RateLimitInfo::from_headers(&headers) {
                                if rate_limit_info.is_rate_limited() {
                                    return Some(Box::pin(sleep(
                                        rate_limit_info
                                            .time_until_reset()
                                            .to_std()
                                            .expect("Negative duration is invalid"),
                                    )));
                                }
                            }
                            None
                        }
                        _ => None,
                    },
                    Err(_) => Some(Box::pin(future::ready(()))),
                }
            }
        }
    }

    fn clone_request(&mut self, req: &Request<OctoBody>) -> Option<Request<OctoBody>> {
        match self {
            RetryConfig::None => None,
            _ => {
                // `Request` can't be cloned
                let mut new_req = Request::builder()
                    .uri(req.uri())
                    .method(req.method())
                    .version(req.version());
                for (name, value) in req.headers() {
                    new_req = new_req.header(name, value);
                }

                let body = req.body().clone();
                let new_req = new_req.body(body).expect(
                    "This should never panic, as we are cloning components from existing request",
                );
                Some(new_req)
            }
        }
    }
}

/// Information about GitHub API rate limits
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct RateLimitInfo {
    /// Maximum number of requests allowed in a time period
    limit: u32,
    /// Number of requests remaining in the current time period
    remaining: u32,
    /// Time when the current rate limit window resets
    reset_time: DateTime<Utc>,
    /// Number of requests used in the current time limit window
    used: u32,
}

#[allow(dead_code)]
impl RateLimitInfo {
    /// Create a new RateLimitInfo from HTTP response headers
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let limit = headers
            .get("x-ratelimit-limit")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok())?;

        let remaining = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok())?;

        let reset = headers
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .map(|timestamp| {
                DateTime::from_timestamp(timestamp, 0).unwrap_or_else(|| Utc::now())
            })?;

        let used = headers
            .get("x-ratelimit-used")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);

        Some(RateLimitInfo {
            limit,
            remaining,
            reset_time: reset,
            used,
        })
    }

    /// Time when the current rate limit window resets
    pub fn reset_time(&self) -> DateTime<Utc> {
        self.reset_time
    }

    /// Check if we're close to hitting the rate limit
    pub fn is_near_limit(&self, threshold: f32) -> bool {
        (self.remaining as f32 / self.limit as f32) < threshold
    }

    /// Calculate time until rate limit reset
    pub fn time_until_reset(&self) -> Duration {
        let now = Utc::now();
        if self.reset_time > now {
            self.reset_time - now
        } else {
            Duration::zero()
        }
    }

    /// Check if we are currently rate limited
    pub fn is_rate_limited(&self) -> bool {
        self.remaining == 0 && self.time_until_reset() > Duration::zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderValue, Response as HttpResponse};
    use std::time::Duration as StdDuration;

    fn create_test_headers(limit: u32, remaining: u32, reset: i64, used: u32) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit", HeaderValue::from(limit));
        headers.insert("x-ratelimit-remaining", HeaderValue::from(remaining));
        headers.insert("x-ratelimit-reset", HeaderValue::from(reset));
        headers.insert("x-ratelimit-used", HeaderValue::from(used));
        headers
    }

    #[test]
    fn test_rate_limit_parsing() {
        let headers = create_test_headers(5000, 4000, Utc::now().timestamp() + 3600, 1000);
        let info = RateLimitInfo::from_headers(&headers).unwrap();

        assert_eq!(info.limit, 5000);
        assert_eq!(info.remaining, 4000);
        assert_eq!(info.used, 1000);
        assert!(info.time_until_reset() > Duration::zero());
    }

    #[test]
    fn test_near_limit_threshold() {
        let headers = create_test_headers(100, 5, Utc::now().timestamp() + 3600, 95);
        let info = RateLimitInfo::from_headers(&headers).unwrap();

        assert!(info.is_near_limit(0.1)); // 5% remaining < 10% threshold
        assert!(!info.is_near_limit(0.01)); // 5% remaining > 1% threshold
    }

    #[tokio::test]
    async fn test_no_retry_on_success() {
        let mut policy = RetryConfig::Simple(3);
        let mut req = Request::builder()
            .uri("https://api.github.com")
            .body(OctoBody::empty())
            .unwrap();
        let mut result = Ok(HttpResponse::builder().status(200).body(()).unwrap());

        assert!(policy.retry(&mut req, &mut result).is_none());
    }

    #[tokio::test]
    async fn test_immediate_retry_on_500() {
        tokio::time::pause();

        let mut policy = RetryConfig::Simple(3);
        let mut req = Request::builder()
            .uri("https://api.github.com")
            .body(OctoBody::empty())
            .unwrap();
        let mut result = Ok(HttpResponse::builder().status(500).body(()).unwrap());

        let retry_future = policy
            .retry(&mut req, &mut result)
            .expect("Should retry on 500");

        let retry_handle = tokio::spawn(retry_future);

        tokio::task::yield_now().await;
        assert!(
            retry_handle.is_finished(),
            "Future should be ready without advancing time"
        );
    }

    #[tokio::test]
    async fn test_delayed_retry_on_rate_limit() {
        tokio::time::pause();

        let mut policy = RetryConfig::Simple(3);
        let mut req = Request::builder()
            .uri("https://api.github.com")
            .body(OctoBody::empty())
            .unwrap();

        let reset_time = (Utc::now() + Duration::seconds(1)).timestamp();

        let mut result = Ok(HttpResponse::builder()
            .status(429)
            .header("x-ratelimit-limit", "100")
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", reset_time.to_string())
            .header("x-ratelimit-used", "100")
            .body(())
            .unwrap());

        let retry_future = policy
            .retry(&mut req, &mut result)
            .expect("Should retry on 429");

        // Start the retry future
        let retry_handle = tokio::spawn(retry_future);
        tokio::task::yield_now().await;
        assert!(
            !retry_handle.is_finished(),
            "Future should not be ready before time is advanced"
        );

        // Advance time by 1 second
        tokio::time::advance(StdDuration::from_secs(1)).await;

        // The future should complete now
        retry_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_no_retry_when_missing_rate_limit_keys() {
        let mut policy = RetryConfig::Simple(3);
        let mut req = Request::builder()
            .uri("https://api.github.com")
            .body(OctoBody::empty())
            .unwrap();

        let mut result = Ok(HttpResponse::builder()
            .status(429)
            .header("x-ratelimit-limit", "100")
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", Utc::now().timestamp().to_string())
            .body(())
            .unwrap());

        assert!(policy.retry(&mut req, &mut result).is_none());
    }
}
