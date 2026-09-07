//! OpenTelemetry receiver: the official hook Claude Code, Codex and Gemini
//! CLI expose for usage telemetry.
//!
//! Each tool can be told, in its own config, to push OTLP/HTTP JSON to a URL.
//! `mtop setup` writes that config; this module is the URL. Only named log
//! events and metrics are read, and only their numeric fields and short
//! labels. Attributes that may carry prompt, response or tool text are never
//! looked at, and account identifiers are never stored.

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
/// Records read from one export; the rest are ignored.
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
            .route("/v1/metrics", post(metrics))
            // Accepted and dropped, so an exporter configured for traces never
            // logs an error. Nothing in them is read.
            .route("/v1/traces", post(accept))
            .layer(DefaultBodyLimit::max(MAX_BODY))
            .with_state(self)
    }
}

async fn accept() -> StatusCode {
    StatusCode::OK
}

/// One thing a log record said.
pub enum Event {
    Request(Box<RequestMetric>),
    Tool {
        name: String,
        duration_ms: Option<f64>,
        ok: bool,
    },
    Count(String, f64),
}

async fn logs(State(r): State<Receiver>, body: String) -> StatusCode {
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut store = r.store.lock().unwrap();
    for event in parse_export(&v) {
        match event {
            Event::Request(m) => {
                let mut m = *m;
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
            Event::Tool {
                name,
                duration_ms,
                ok,
            } => store.tool(&name, duration_ms, ok),
            Event::Count(key, by) => store.count(&key, by),
        }
    }
    StatusCode::OK
}

async fn metrics(State(r): State<Receiver>, body: String) -> StatusCode {
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let mut store = r.store.lock().unwrap();
    for (key, by) in parse_metrics(&v) {
        store.count(&key, by);
    }
    StatusCode::OK
}

/// Every event found in one OTLP JSON logs export.
pub fn parse_export(v: &Value) -> Vec<Event> {
    v["resourceLogs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r["scopeLogs"].as_array().into_iter().flatten())
        .flat_map(|s| s["logRecords"].as_array().into_iter().flatten())
        .take(MAX_RECORDS)
        .filter_map(parse_record)
        .collect()
}

/// Counters worth keeping from one OTLP JSON metrics export: the value of
/// every data point of a named metric, summed, keyed by metric name and
/// the `type` attribute when there is one (lines of code added or removed).
pub fn parse_metrics(v: &Value) -> Vec<(String, f64)> {
    const KEEP: [&str; 6] = [
        "claude_code.active_time.total",
        "claude_code.lines_of_code.count",
        "claude_code.commit.count",
        "claude_code.pull_request.count",
        "claude_code.session.count",
        "codex.tool.call",
    ];
    let mut out = vec![];
    let metrics = v["resourceMetrics"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|r| r["scopeMetrics"].as_array().into_iter().flatten())
        .flat_map(|s| s["metrics"].as_array().into_iter().flatten())
        .take(MAX_RECORDS);
    for metric in metrics {
        let Some(name) = metric["name"].as_str() else {
            continue;
        };
        if !KEEP.contains(&name) {
            continue;
        }
        let points = ["sum", "gauge"]
            .iter()
            .flat_map(|k| metric[*k]["dataPoints"].as_array().into_iter().flatten());
        for point in points {
            let value = point["asDouble"]
                .as_f64()
                .or_else(|| point["asInt"].as_str().and_then(|s| s.parse().ok()))
                .or_else(|| point["asInt"].as_f64());
            let Some(value) = value.filter(|n| n.is_finite()) else {
                continue;
            };
            let key = match text(point, "type") {
                Some(t) => format!("{name}.{t}"),
                None => name.to_string(),
            };
            out.push((key, value));
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
    let v = attr(record, key)?;
    v["stringValue"]
        .as_str()
        .map(safe_label)
        .or_else(|| v["boolValue"].as_bool().map(|b| b.to_string()))
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

fn flag(record: &Value, key: &str) -> Option<bool> {
    match text(record, key)?.as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn label(record: &Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|k| text(record, k).filter(|v| !v.is_empty()))
        .next()
        .unwrap_or_default()
}

fn tier(record: &Value, keys: &[&str]) -> String {
    let parts: Vec<String> = keys
        .iter()
        .filter_map(|k| text(record, k).filter(|v| !v.is_empty()))
        .collect();
    parts.join("/")
}

fn id_for(key: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    // High bit: not a proxy sequence number. Second bit: not a tailed record.
    h.finish() | (1 << 63) | (1 << 62)
}

fn parse_record(record: &Value) -> Option<Event> {
    let name = text(record, "event.name")
        .or_else(|| record["body"]["stringValue"].as_str().map(safe_label))?;
    let time = record["timeUnixNano"].as_str().unwrap_or_default();
    let session = text(record, "session.id")
        .map(|s| s.chars().take(8).collect())
        .unwrap_or_default();
    let base = RequestMetric {
        status: "telemetry".into(),
        model: text(record, "model").unwrap_or_else(|| "unknown".into()),
        session,
        ..Default::default()
    };
    let event = match name.as_str() {
        "claude_code.api_request" | "api_request" => {
            let input = count(record, "input_tokens")?;
            let request_id = text(record, "request_id").unwrap_or_default();
            Event::Request(Box::new(RequestMetric {
                id: id_for(&("claude", request_id.as_str(), time)),
                provider: "claude-code".into(),
                usage: Usage {
                    input: Some(input),
                    output: count(record, "output_tokens"),
                    cache_read: count(record, "cache_read_tokens"),
                    cache_write: count(record, "cache_creation_tokens"),
                },
                duration_ms: num(record, "duration_ms"),
                // The tool's own estimate, reported as it sent it.
                estimated_cost_usd: num(record, "cost_usd"),
                source: text(record, "query_source").unwrap_or_default(),
                agent: label(record, &["agent.name", "skill.name", "plugin.name"]),
                tier: tier(record, &["speed", "effort"]),
                ..base
            }))
        }
        "claude_code.api_error" | "api_error" => Event::Request(Box::new(RequestMetric {
            id: id_for(&("claude-error", time)),
            provider: "claude-code".into(),
            status: "api error".into(),
            duration_ms: num(record, "duration_ms"),
            http_status: count(record, "status_code").map(|s| s.min(999) as u16),
            attempt: count(record, "attempt"),
            error: text(record, "error").unwrap_or_else(|| "api error".into()),
            tier: tier(record, &["speed", "effort"]),
            ..base
        })),
        "claude_code.tool_result" | "tool_result" => Event::Tool {
            name: label(record, &["tool_name"]),
            duration_ms: num(record, "duration_ms"),
            ok: flag(record, "success").unwrap_or(true),
        },
        "claude_code.tool_decision" | "tool_decision" => Event::Count(
            format!(
                "claude_code.tool_decision.{}",
                text(record, "decision").unwrap_or_else(|| "unknown".into())
            ),
            1.,
        ),
        "codex.sse_event" => {
            if text(record, "event.kind").as_deref() != Some("response.completed") {
                return None;
            }
            let input = count(record, "input_token_count")?;
            Event::Request(Box::new(RequestMetric {
                id: id_for(&("codex", time, input)),
                provider: "codex".into(),
                usage: Usage {
                    input: Some(input),
                    output: count(record, "output_token_count"),
                    cache_read: count(record, "cached_token_count"),
                    cache_write: count(record, "cache_write_token_count"),
                },
                reasoning: count(record, "reasoning_token_count").filter(|n| *n > 0),
                ttft_ms: num(record, "ttft_ms"),
                tier: tier(record, &["service_tier", "model_reasoning_effort"]),
                ..base
            }))
        }
        "codex.api_request" => {
            // Success is reported by the sse_event that follows; only a
            // failed exchange is a request of its own.
            let status = count(record, "http.response.status_code").map(|s| s.min(999) as u16);
            let error = text(record, "error.message").unwrap_or_default();
            if error.is_empty() && !status.is_some_and(|s| s >= 400) {
                return None;
            }
            Event::Request(Box::new(RequestMetric {
                id: id_for(&("codex-error", time)),
                provider: "codex".into(),
                status: "api error".into(),
                duration_ms: num(record, "duration_ms"),
                http_status: status,
                attempt: count(record, "attempt"),
                error: if error.is_empty() {
                    "api error".into()
                } else {
                    error
                },
                ..base
            }))
        }
        "codex.tool_result" => Event::Tool {
            name: label(record, &["tool_name"]),
            duration_ms: num(record, "duration_ms"),
            ok: flag(record, "success").unwrap_or(true),
        },
        "gemini_cli.api_response" => {
            let input = count(record, "input_token_count")?;
            let prompt_id = text(record, "prompt_id").unwrap_or_default();
            Event::Request(Box::new(RequestMetric {
                id: id_for(&("gemini", prompt_id.as_str(), time)),
                provider: "gemini-cli".into(),
                usage: Usage {
                    input: Some(input),
                    output: count(record, "output_token_count"),
                    cache_read: count(record, "cached_content_token_count"),
                    cache_write: None,
                },
                reasoning: count(record, "thoughts_token_count").filter(|n| *n > 0),
                duration_ms: num(record, "duration_ms"),
                ..base
            }))
        }
        "gemini_cli.api_error" => Event::Request(Box::new(RequestMetric {
            id: id_for(&("gemini-error", time)),
            provider: "gemini-cli".into(),
            status: "api error".into(),
            duration_ms: num(record, "duration_ms"),
            http_status: count(record, "status_code").map(|s| s.min(999) as u16),
            attempt: count(record, "attempt"),
            error: text(record, "error").unwrap_or_else(|| "api error".into()),
            ..base
        })),
        "gemini_cli.tool_call" => Event::Tool {
            name: label(record, &["function_name"]),
            duration_ms: num(record, "duration_ms"),
            ok: flag(record, "success").unwrap_or(true),
        },
        _ => return None,
    };
    Some(event)
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
    fn s(v: &str) -> Value {
        json!({"stringValue": v})
    }
    fn requests(events: Vec<Event>) -> Vec<RequestMetric> {
        events
            .into_iter()
            .filter_map(|e| match e {
                Event::Request(m) => Some(*m),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn maps_all_three_tools_and_number_encodings() {
        let v = export(vec![
            rec(
                "claude_code.api_request",
                vec![
                    ("model", s("claude-fable-5-1")),
                    ("input_tokens", json!({"intValue":"32"})),
                    ("output_tokens", json!({"intValue":208})),
                    ("cache_read_tokens", json!({"doubleValue":60875.0})),
                    ("cost_usd", json!({"doubleValue":0.0123})),
                    ("duration_ms", json!({"intValue":"1500"})),
                    ("request_id", s("req_1")),
                    ("query_source", s("subagent")),
                    ("skill.name", s("tdd")),
                    ("speed", s("fast")),
                    ("effort", s("high")),
                    ("session.id", s("abcdef12-9999")),
                    ("prompt", s("never read")),
                ],
            ),
            rec(
                "log",
                vec![
                    ("event.name", s("codex.sse_event")),
                    ("event.kind", s("response.completed")),
                    ("model", s("gpt-5.5")),
                    ("input_token_count", s("19627")),
                    ("output_token_count", s("341")),
                    ("cached_token_count", json!({"intValue":2432})),
                    ("reasoning_token_count", json!({"intValue":19})),
                    ("ttft_ms", json!({"intValue":800})),
                    ("service_tier", s("priority")),
                ],
            ),
            rec(
                "log",
                vec![
                    ("event.name", s("codex.sse_event")),
                    ("event.kind", s("response.output_item.done")),
                ],
            ),
            rec(
                "gemini_cli.api_response",
                vec![
                    ("model", s("gemini-3-pro")),
                    ("input_token_count", json!({"intValue":100})),
                    ("output_token_count", json!({"intValue":10})),
                    ("cached_content_token_count", json!({"intValue":50})),
                ],
            ),
            rec("claude_code.user_prompt", vec![("prompt", s("x"))]),
        ]);
        let out = requests(parse_export(&v));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].provider, "claude-code");
        assert_eq!(out[0].usage.input, Some(32));
        assert_eq!(out[0].usage.cache_read, Some(60875));
        assert_eq!(out[0].estimated_cost_usd, Some(0.0123));
        assert_eq!(out[0].duration_ms, Some(1500.));
        assert_eq!(out[0].source, "subagent");
        assert_eq!(out[0].agent, "tdd");
        assert_eq!(out[0].tier, "fast/high");
        assert_eq!(out[0].session, "abcdef12");
        assert_eq!(out[1].provider, "codex");
        assert_eq!(out[1].usage.input, Some(19627));
        assert_eq!(out[1].reasoning, Some(19));
        assert_eq!(out[1].ttft_ms, Some(800.));
        assert_eq!(out[1].tier, "priority");
        assert_eq!(out[2].provider, "gemini-cli");
        assert_eq!(out[2].usage.cache_read, Some(50));
        assert!(out.iter().all(|m| m.id >> 62 == 0b11));
    }

    #[test]
    fn errors_tools_and_decisions() {
        let v = export(vec![
            rec(
                "claude_code.api_error",
                vec![
                    ("model", s("claude-fable-5-1")),
                    ("status_code", json!({"intValue":529})),
                    ("attempt", json!({"intValue":2})),
                    ("error", s("overloaded")),
                ],
            ),
            rec(
                "claude_code.tool_result",
                vec![
                    ("tool_name", s("Bash")),
                    ("duration_ms", json!({"intValue":42})),
                    ("success", json!({"boolValue":false})),
                ],
            ),
            rec("claude_code.tool_decision", vec![("decision", s("accept"))]),
            rec(
                "log",
                vec![
                    ("event.name", s("codex.api_request")),
                    ("http.response.status_code", json!({"intValue":200})),
                ],
            ),
            rec(
                "log",
                vec![
                    ("event.name", s("codex.api_request")),
                    ("http.response.status_code", json!({"intValue":429})),
                    ("error.message", s("usage limit")),
                ],
            ),
            rec(
                "gemini_cli.tool_call",
                vec![
                    ("function_name", s("read_file")),
                    ("duration_ms", json!({"intValue":5})),
                    ("success", json!({"boolValue":true})),
                ],
            ),
        ]);
        let events = parse_export(&v);
        assert_eq!(events.len(), 5, "a 200 api_request is not an event");
        assert!(
            matches!(&events[0], Event::Request(m) if m.http_status == Some(529) && m.attempt == Some(2) && m.error == "overloaded")
        );
        assert!(
            matches!(&events[1], Event::Tool { name, duration_ms: Some(ms), ok: false } if name == "Bash" && *ms == 42.)
        );
        assert!(
            matches!(&events[2], Event::Count(k, 1.0) if k == "claude_code.tool_decision.accept")
        );
        assert!(
            matches!(&events[3], Event::Request(m) if m.provider == "codex" && m.http_status == Some(429))
        );
        assert!(matches!(&events[4], Event::Tool { name, ok: true, .. } if name == "read_file"));
    }

    #[test]
    fn metrics_keep_named_counters_only() {
        let v = json!({"resourceMetrics":[{"scopeMetrics":[{"metrics":[
            {"name":"claude_code.lines_of_code.count","sum":{"dataPoints":[
                {"asInt":"12","attributes":[{"key":"type","value":{"stringValue":"added"}}]},
                {"asInt":"3","attributes":[{"key":"type","value":{"stringValue":"removed"}}]}]}},
            {"name":"claude_code.active_time.total","sum":{"dataPoints":[{"asDouble":90.5}]}},
            {"name":"claude_code.cost.usage","sum":{"dataPoints":[{"asDouble":1.0}]}}
        ]}]}]});
        let out = parse_metrics(&v);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0],
            ("claude_code.lines_of_code.count.added".into(), 12.)
        );
        assert_eq!(out[2], ("claude_code.active_time.total".into(), 90.5));
    }

    #[test]
    fn malformed_export_yields_nothing() {
        assert!(parse_export(&json!({"resourceLogs": "no"})).is_empty());
        assert!(parse_export(&json!([])).is_empty());
        assert!(parse_metrics(&json!([])).is_empty());
    }
}
