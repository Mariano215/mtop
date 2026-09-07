use axum::{
    Router,
    body::Body,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
};
use mtop::{
    model::Store,
    proxy::{Proxy, Timeouts},
};
use std::time::Duration;

async fn bind(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, task)
}

#[tokio::test]
async fn preserves_stream_body_auth_status_and_usage() {
    const PAYLOAD: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: {\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
    let upstream = Router::new().route(
        "/v1/chat/completions",
        post(|headers: HeaderMap, body: String| async move {
            assert_eq!(headers["authorization"], "Bearer test-only");
            assert_eq!(body, "{\"model\":\"test\",\"stream\":true}");
            let chunks = async_stream::stream! {
                for chunk in PAYLOAD.as_bytes().chunks(7) {
                    yield Ok::<_, std::io::Error>(chunk.to_vec());
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            };
            Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .header("x-request-id", "fixture")
                .body(Body::from_stream(chunks))
                .unwrap()
        }),
    );
    let (url, upstream_task) = bind(upstream).await;
    let store = Store::shared(10);
    let (proxy_url, proxy_task) = bind(
        Proxy::new(store.clone(), &url, "openai", vec![], Timeouts::default())
            .unwrap()
            .router(),
    )
    .await;
    let response = reqwest::Client::new()
        .post(format!("{proxy_url}/v1/chat/completions"))
        .header("authorization", "Bearer test-only")
        .body("{\"model\":\"test\",\"stream\":true}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-request-id"], "fixture");
    assert_eq!(response.text().await.unwrap(), PAYLOAD);
    let s = store.lock().unwrap();
    assert_eq!(s.completed, 1);
    let r = &s.requests[0];
    assert_eq!(r.usage.input, Some(8));
    assert_eq!(r.usage.output, Some(2));
    assert_eq!(r.status, "complete");
    assert!(r.ttft_ms.is_some());
    assert!(r.estimated_cost_usd.is_none());
    upstream_task.abort();
    proxy_task.abort();
}

#[tokio::test]
async fn preserves_http_error() {
    let upstream = Router::new().fallback(|| async {
        Response::builder()
            .status(429)
            .header("retry-after", "5")
            .body(Body::from("quota"))
            .unwrap()
    });
    let (url, u) = bind(upstream).await;
    let s = Store::shared(10);
    let (url, p) = bind(
        Proxy::new(s.clone(), &url, "openai", vec![], Timeouts::default())
            .unwrap()
            .router(),
    )
    .await;
    let r = reqwest::get(url).await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers()["retry-after"], "5");
    assert_eq!(r.text().await.unwrap(), "quota");
    assert_eq!(s.lock().unwrap().requests[0].status, "HTTP error");
    p.abort();
    u.abort();
}

#[tokio::test]
async fn does_not_follow_redirect() {
    let upstream = Router::new().fallback(|| async {
        Response::builder()
            .status(302)
            .header("location", "http://127.0.0.1:1/never")
            .body(Body::empty())
            .unwrap()
    });
    let (url, u) = bind(upstream).await;
    let s = Store::shared(10);
    let (url, p) = bind(
        Proxy::new(s, &url, "openai", vec![], Timeouts::default())
            .unwrap()
            .router(),
    )
    .await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = client.get(url).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(r.headers()["location"], "http://127.0.0.1:1/never");
    p.abort();
    u.abort();
}

#[tokio::test]
async fn rejects_oversized_request_before_forwarding() {
    let s = Store::shared(10);
    let (url, p) = bind(
        Proxy::new(
            s.clone(),
            "http://127.0.0.1:1",
            "openai",
            vec![],
            Timeouts::default(),
        )
        .unwrap()
        .router(),
    )
    .await;
    let r = reqwest::Client::new()
        .post(url)
        .body(vec![b'x'; 4 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(s.lock().unwrap().completed, 0);
    p.abort();
}

#[tokio::test]
async fn honors_configured_upstream_timeout() {
    let upstream = Router::new().fallback(|| async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        "never sent"
    });
    let (url, u) = bind(upstream).await;
    let s = Store::shared(10);
    let timeouts = Timeouts {
        upstream: Duration::from_millis(150),
        ..Timeouts::default()
    };
    let (url, p) = bind(
        Proxy::new(s.clone(), &url, "openai", vec![], timeouts)
            .unwrap()
            .router(),
    )
    .await;
    let started = std::time::Instant::now();
    let r = reqwest::Client::new()
        .post(url)
        .body("{}")
        .send()
        .await
        .unwrap();
    // The default is 600 s, so finishing this fast proves the configured value is used.
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(s.lock().unwrap().completed, 1);
    p.abort();
    u.abort();
}
