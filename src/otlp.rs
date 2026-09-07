//! OpenTelemetry receiver: the official hook Claude Code, Codex and Gemini
//! CLI expose for usage telemetry.
//!
//! Each tool can be told, in its own config, to push OTLP/HTTP JSON to a URL.
//! `mtop setup` writes that config; this module is the URL. Only three log
//! events are read, and only their numeric fields and model name. Attributes
//! that may carry prompt or response text are never looked at.

use crate::model::{Price, RequestMetric, Shared, Usage, safe_label};
use axum::{
    Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::post,
};
use serde_json::Value;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

/// Largest export accepted. Claude Code batches, but not this much.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Log records read from one export; the rest are ignored.
const MAX_RECORDS: usize = 10_000;

#[derive(Clone)]
pub struct Receiver {
    store: Shared,
    prices: Arc<Vec<Price>>,
}

impl Receiver {
    pub fn new(store: Shared, prices: Vec<Price>) -> Self {
        Self {
            store,
            prices: Arc::new(prices),
        }
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/v1/logs", post(logs))
            // Accepted and dropped, so an exporter configured for all signals
            // never logs an error. Nothing in them is read.
            .route("/v1/metrics", post(accept))
            .route("/v1/traces", post(accept))
            .layer(DefaultBodyLimit::max(MAX_BODY))
            .with_state(self)
    }
}

async fn accept() -> StatusCode {
    StatusCode::OK
}

async fn logs(State(r): State<Receiver>, body: String) -> StatusCode {
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut store = r.store.lock().unwrap();
    for m in parse_export(&v) {
        let mut m = m;
        if m.estimated_cost_usd.is_none() {
            m.estimated_cost_usd = r
                .prices
                .iter()
                .find(|p| p.model == m.model)
                .and_then(|p| p.cost(&m.provider, &m.usage));
        }
        *store.telemetry.entry(m.provider.clone()).or_default() += 1;
        store.finish(m);
    }
    StatusCode::OK
}

/// Every request found in one OTLP JSON logs export.
pub fn parse_export(v: &Value) -> Vec<RequestMetric> {
    let mut out = vec![];
    let records = v["resourceLogs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r["scopeLogs"].as_array().into_iter().flatten())
        .flat_map(|s| s["logRecords"].as_array().into_iter().flatten())
        .take(MAX_RECORDS);
    for record in records {
        if let Some(m) = parse_record(record) {
            out.push(m);
        }
    }
    out
}

/// One attribute value, whatever OTLP encoding it arrived in.
fn attr<'a>(record: &'a Value, key: &str) -> Option<&'a Value> {
    record["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)
        .map(|a| &a["value"])
}

fn text(record: &Value, key: &str) -> Option<String> {
    attr(record, key)?["stringValue"].as_str().map(safe_label)
}

/// OTLP JSON writes integers as strings, and Codex formats some counts with
/// Display, so a number may arrive as intValue "32", intValue 32, doubleValue
/// 32.0 or stringValue "32". All four are the same number.
fn num(record: &Value, key: &str) -> Option<f64> {
    let v = attr(record, key)?;
    for enc in ["intValue", "doubleValue", "stringValue"] {
        match &v[enc] {
            Value::Number(n) => return n.as_f64(),
            Value::String(s) => return s.trim().parse().ok(),
            _ => (),
        }
    }
    None
}

fn count(record: &Value, key: &str) -> Option<u64> {
    num(record, key).filter(|n| *n >= 0.).map(|n| n as u64)
}

fn id_for(key: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    // High bit: not a proxy sequence number. Second bit: not a tailed record.
    h.finish() | (1 << 63) | (1 << 62)
}

fn parse_record(record: &Value) -> Option<RequestMetric> {
    let name = text(record, "event.name")
        .or_else(|| record["body"]["stringValue"].as_str().map(safe_label))?;
    let time = record["timeUnixNano"].as_str().unwrap_or_default();
    match name.as_str() {
        "claude_code.api_request" | "api_request" => {
            let input = count(record, "input_tokens")?;
            let request_id = text(record, "request_id").unwrap_or_default();
            Some(RequestMetric {
                id: id_for(&("claude", request_id.as_str(), time)),
                provider: "claude-code".into(),
                model: text(record, "model").unwrap_or_else(|| "unknown".into()),
                status: "telemetry".into(),
                usage: Usage {
                    input: Some(input),
                    output: count(record, "output_tokens"),
                    cache_read: count(record, "cache_read_tokens"),
                    cache_write: count(record, "cache_creation_tokens"),
                },
                duration_ms: num(record, "duration_ms"),
                // The tool's own estimate, reported as it sent it.
                estimated_cost_usd: num(record, "cost_usd"),
                ..Default::default()
            })
        }
        "codex.sse_event" => {
            if text(record, "event.kind").as_deref() != Some("response.completed") {
                return None;
            }
            let input = count(record, "input_token_count")?;
            Some(RequestMetric {
                id: id_for(&("codex", time, input)),
                provider: "codex".into(),
                model: text(record, "model").unwrap_or_else(|| "unknown".into()),
                status: "telemetry".into(),
                usage: Usage {
                    input: Some(input),
                    output: count(record, "output_token_count"),
                    cache_read: count(record, "cached_token_count"),
                    cache_write: count(record, "cache_write_token_count"),
                },
                ttft_ms: num(record, "ttft_ms"),
                ..Default::default()
            })
        }
        "gemini_cli.api_response" => {
            let input = count(record, "input_token_count")?;
            let prompt_id = text(record, "prompt_id").unwrap_or_default();
            Some(RequestMetric {
                id: id_for(&("gemini", prompt_id.as_str(), time)),
                provider: "gemini-cli".into(),
                model: text(record, "model").unwrap_or_else(|| "unknown".into()),
                status: "telemetry".into(),
                usage: Usage {
                    input: Some(input),
                    output: count(record, "output_token_count"),
                    cache_read: count(record, "cached_content_token_count"),
                    cache_write: None,
                },
                duration_ms: num(record, "duration_ms"),
                ..Default::default()
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn export(records: Vec<Value>) -> Value {
        json!({"resourceLogs":[{"scopeLogs":[{"logRecords":records}]}]})
    }
    fn rec(name: &str, attrs: Vec<(&str, Value)>) -> Value {
        let attributes: Vec<Value> = attrs
            .into_iter()
            .map(|(k, v)| json!({"key": k, "value": v}))
            .collect();
        json!({"timeUnixNano":"1700000000000000000","body":{"stringValue":name},"attributes":attributes})
    }

    #[test]
    fn maps_all_three_tools_and_number_encodings() {
        let v = export(vec![
            rec(
                "claude_code.api_request",
                vec![
                    ("model", json!({"stringValue":"claude-fable-5-1"})),
                    ("input_tokens", json!({"intValue":"32"})),
                    ("output_tokens", json!({"intValue":208})),
                    ("cache_read_tokens", json!({"doubleValue":60875.0})),
                    ("cost_usd", json!({"doubleValue":0.0123})),
                    ("duration_ms", json!({"intValue":"1500"})),
                    ("request_id", json!({"stringValue":"req_1"})),
                    ("prompt", json!({"stringValue":"never read"})),
                ],
            ),
            rec(
                "log",
                vec![
                    ("event.name", json!({"stringValue":"codex.sse_event"})),
                    ("event.kind", json!({"stringValue":"response.completed"})),
                    ("model", json!({"stringValue":"gpt-5.5"})),
                    ("input_token_count", json!({"stringValue":"19627"})),
                    ("output_token_count", json!({"stringValue":"341"})),
                    ("cached_token_count", json!({"intValue":2432})),
                ],
            ),
            rec(
                "log",
                vec![
                    ("event.name", json!({"stringValue":"codex.sse_event"})),
                    (
                        "event.kind",
                        json!({"stringValue":"response.output_item.done"}),
                    ),
                ],
            ),
            rec(
                "gemini_cli.api_response",
                vec![
                    ("model", json!({"stringValue":"gemini-3-pro"})),
                    ("input_token_count", json!({"intValue":100})),
                    ("output_token_count", json!({"intValue":10})),
                    ("cached_content_token_count", json!({"intValue":50})),
                ],
            ),
            rec(
                "claude_code.user_prompt",
                vec![("prompt", json!({"stringValue":"x"}))],
            ),
        ]);
        let out = parse_export(&v);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].provider, "claude-code");
        assert_eq!(out[0].usage.input, Some(32));
        assert_eq!(out[0].usage.output, Some(208));
        assert_eq!(out[0].usage.cache_read, Some(60875));
        assert_eq!(out[0].estimated_cost_usd, Some(0.0123));
        assert_eq!(out[0].duration_ms, Some(1500.));
        assert_eq!(out[1].provider, "codex");
        assert_eq!(out[1].model, "gpt-5.5");
        assert_eq!(out[1].usage.input, Some(19627));
        assert_eq!(out[1].usage.cache_read, Some(2432));
        assert_eq!(out[2].provider, "gemini-cli");
        assert_eq!(out[2].usage.cache_read, Some(50));
        assert!(out.iter().all(|m| m.id >> 62 == 0b11));
    }

    #[test]
    fn malformed_export_yields_nothing() {
        assert!(parse_export(&json!({"resourceLogs": "no"})).is_empty());
        assert!(parse_export(&json!([])).is_empty());
    }
}
