use crate::{
    model::{Price, RequestMetric, Shared, safe_label},
    parser::Observer,
    poller,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

/// Wall-clock limits for one forwarded request.
#[derive(Clone, Copy)]
pub struct Timeouts {
    /// Reading the client request body before forwarding starts.
    pub body: Duration,
    /// The whole upstream exchange, including a long streamed response.
    pub upstream: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            body: Duration::from_secs(30),
            upstream: Duration::from_secs(600),
        }
    }
}

#[derive(Clone)]
pub struct Proxy {
    store: Shared,
    client: reqwest::Client,
    upstream: String,
    provider: String,
    prices: Arc<Vec<Price>>,
    sequence: Arc<AtomicU64>,
    slots: Arc<Semaphore>,
    timeouts: Timeouts,
}

impl Proxy {
    pub fn new(
        store: Shared,
        upstream: &str,
        provider: &str,
        prices: Vec<Price>,
        timeouts: Timeouts,
    ) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(upstream)?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "upstream must be HTTP(S)"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "upstream must not contain credentials, query or fragment"
        );
        Ok(Self {
            store,
            client: poller::client()?,
            upstream: upstream.trim_end_matches('/').into(),
            provider: provider.into(),
            prices: Arc::new(prices),
            sequence: Arc::new(AtomicU64::new(1)),
            slots: Arc::new(Semaphore::new(16)),
            timeouts,
        })
    }
    pub fn router(self) -> Router {
        Router::new().fallback(forward).with_state(self)
    }
}

fn strip_headers(headers: &mut HeaderMap) {
    let connection = headers
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    for name in connection
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        headers.remove(name);
    }
    for name in [
        "host",
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "content-length",
    ] {
        headers.remove(name);
    }
}

struct Completion {
    store: Shared,
    observer: Observer,
    started: Instant,
    prices: Arc<Vec<Price>>,
    done: bool,
}
impl Completion {
    fn finish(&mut self, status: &str) {
        if self.done {
            return;
        }
        self.done = true;
        self.observer
            .finish(self.started.elapsed().as_secs_f64() * 1000.);
        if self.observer.metric.status != "provider error" {
            self.observer.metric.status = status.into();
        }
        let m = &mut self.observer.metric;
        m.estimated_cost_usd = self
            .prices
            .iter()
            .find(|p| p.model == m.model)
            .and_then(|p| p.cost(&m.provider, &m.usage));
        self.store.lock().unwrap().finish(m.clone());
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.finish("cancelled");
    }
}

async fn forward(State(p): State<Proxy>, request: Request) -> Response {
    let Ok(permit) = p.slots.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "MTop concurrency limit").into_response();
    };
    let (parts, body) = request.into_parts();
    let body = match tokio::time::timeout(p.timeouts.body, to_bytes(body, 4 * 1024 * 1024)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "MTop request limit: 4 MiB").into_response();
        }
        Err(_) => return (StatusCode::REQUEST_TIMEOUT, "MTop request timeout").into_response(),
    };
    let model = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["model"].as_str().map(safe_label))
        .unwrap_or_else(|| "unknown".into());
    let metric = RequestMetric {
        id: p.sequence.fetch_add(1, Ordering::Relaxed),
        model,
        provider: p.provider.clone(),
        status: "connecting".into(),
        ..Default::default()
    };
    p.store.lock().unwrap().update(metric.clone());
    let mut completion = Completion {
        store: p.store.clone(),
        observer: Observer::new(metric, ""),
        started: Instant::now(),
        prices: p.prices.clone(),
        done: false,
    };
    let mut headers = parts.headers;
    strip_headers(&mut headers);
    headers.insert("accept-encoding", "identity".parse().unwrap());
    // Concatenation preserves the configured origin; client absolute URIs cannot select a host.
    let url = format!(
        "{}{}",
        p.upstream,
        parts
            .uri
            .path_and_query()
            .map(|v| v.as_str())
            .unwrap_or("/")
    );
    let result = p
        .client
        .request(parts.method, &url)
        .headers(headers)
        .body(body)
        .timeout(p.timeouts.upstream)
        .send()
        .await;
    let upstream = match result {
        Ok(r) => r,
        Err(_) => {
            completion.finish("upstream error");
            return (StatusCode::BAD_GATEWAY, "MTop upstream request failed").into_response();
        }
    };
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    strip_headers(&mut headers);
    let content_type = headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    completion.observer = Observer::new(completion.observer.metric.clone(), content_type);
    completion.observer.metric.status = "streaming".into();
    let mut stream = upstream.bytes_stream();
    let output = async_stream::stream! {
        let _permit = permit;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    completion.observer.feed(&bytes, completion.started.elapsed().as_secs_f64() * 1000.);
                    completion.store.lock().unwrap().update(completion.observer.metric.clone());
                    yield Ok::<_, std::io::Error>(bytes);
                }
                Err(_) => {
                    completion.finish("stream error");
                    yield Err(std::io::Error::other("upstream stream failed"));
                    return;
                }
            }
        }
        completion.finish(if status.is_success() { "complete" } else { "HTTP error" });
    };
    let mut response = Response::new(Body::from_stream(output));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}
