//! Bounded telemetry observation. Forwarded bytes are never rewritten.
use crate::model::{RequestMetric, safe_label};
use serde_json::Value;
use std::collections::HashSet;

const LIMIT: usize = 256 * 1024;

pub struct Observer {
    pub metric: RequestMetric,
    line: Vec<u8>,
    event: Vec<u8>,
    discard_line: bool,
    discard_event: bool,
    sse: bool,
    json: bool,
    tools: HashSet<String>,
}

impl Observer {
    pub fn new(metric: RequestMetric, content_type: &str) -> Self {
        Self {
            metric,
            line: vec![],
            event: vec![],
            discard_line: false,
            discard_event: false,
            sse: content_type.contains("text/event-stream"),
            json: content_type.contains("application/json"),
            tools: HashSet::new(),
        }
    }
    pub fn feed(&mut self, bytes: &[u8], elapsed_ms: f64) {
        for &byte in bytes {
            if byte == b'\n' && !self.json {
                self.line_done(elapsed_ms);
            } else if !self.discard_line {
                if self.line.len() >= LIMIT {
                    self.line.clear();
                    self.discard_line = true;
                    self.discard_event = true;
                    self.metric.parse_errors += 1;
                } else {
                    self.line.push(byte);
                }
            }
        }
    }
    fn line_done(&mut self, elapsed: f64) {
        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if self.discard_line {
            self.discard_line = false;
            return;
        }
        if self.sse {
            if line.is_empty() {
                if !self.discard_event && !self.event.is_empty() {
                    let event = std::mem::take(&mut self.event);
                    self.parse(&event, elapsed);
                }
                self.event.clear();
                self.discard_event = false;
            } else if let Some(data) = line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                if self.event.len() + data.len() + 1 > LIMIT {
                    if !self.discard_event {
                        self.metric.parse_errors += 1;
                    }
                    self.discard_event = true;
                    self.event.clear();
                } else if !self.discard_event {
                    if !self.event.is_empty() {
                        self.event.push(b'\n');
                    }
                    self.event.extend_from_slice(data);
                }
            }
        } else if !line.is_empty() {
            self.parse(&line, elapsed);
        }
    }
    pub fn finish(&mut self, elapsed: f64) {
        if !self.line.is_empty() {
            self.line_done(elapsed);
        }
        if self.sse && !self.event.is_empty() && !self.discard_event {
            let event = std::mem::take(&mut self.event);
            self.parse(&event, elapsed);
        }
        self.metric.duration_ms = Some(elapsed);
    }
    fn parse(&mut self, data: &[u8], elapsed: f64) {
        if data == b"[DONE]" {
            return;
        }
        match serde_json::from_slice::<Value>(data) {
            Ok(v) => self.observe(&v, elapsed),
            Err(_) => self.metric.parse_errors += 1,
        }
    }
    fn observe(&mut self, v: &Value, elapsed: f64) {
        let kind = v["type"].as_str().unwrap_or("");
        if kind == "error" || v.get("error").is_some() {
            self.metric.status = "provider error".into();
        }
        let root = if kind == "message_start" {
            &v["message"]
        } else if kind.starts_with("response.") && v["response"].is_object() {
            &v["response"]
        } else {
            v
        };
        if let Some(model) = root["model"].as_str() {
            self.metric.model = safe_label(model);
        }
        let usage = &root["usage"];
        macro_rules! count { ($field:ident, $($value:expr),+) => {
            if let Some(n) = [$($value.as_u64()),+].into_iter().flatten().next() { self.metric.usage.$field = Some(n); }
        }; }
        count!(
            input,
            usage["prompt_tokens"],
            usage["input_tokens"],
            root["prompt_eval_count"]
        );
        count!(
            output,
            usage["completion_tokens"],
            usage["output_tokens"],
            root["eval_count"]
        );
        count!(
            cache_read,
            usage["cache_read_input_tokens"],
            usage["prompt_tokens_details"]["cached_tokens"],
            usage["input_tokens_details"]["cached_tokens"],
            root["prompt_eval_cached_count"]
        );
        count!(cache_write, usage["cache_creation_input_tokens"]);
        if let (Some(n), Some(ns)) = (root["eval_count"].as_f64(), root["eval_duration"].as_f64())
            && ns > 0.
        {
            self.metric.generation_tps = Some(n / ns * 1e9);
        }
        let mut content = v["response"].as_str().is_some_and(|s| !s.is_empty())
            || v["message"]["content"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            || v["delta"]["text"].as_str().is_some_and(|s| !s.is_empty())
            || (kind == "response.output_text.delta"
                && v["delta"].as_str().is_some_and(|s| !s.is_empty()));
        if let Some(choices) = v["choices"].as_array() {
            for choice in choices {
                let delta = &choice["delta"];
                content |= delta["content"].as_str().is_some_and(|s| !s.is_empty());
                if let Some(calls) = delta["tool_calls"].as_array() {
                    for call in calls {
                        let key = format!("chat:{}:{}", choice["index"], call["index"]);
                        self.tool(key);
                    }
                }
                if let Some(calls) = choice["message"]["tool_calls"].as_array() {
                    for (i, _) in calls.iter().enumerate() {
                        self.tool(format!("chat:{}:{i}", choice["index"]));
                    }
                }
            }
        }
        if kind == "content_block_start" && v["content_block"]["type"] == "tool_use" {
            self.tool(format!("anthropic:{}", v["index"]));
        }
        if kind == "response.output_item.added" && v["item"]["type"] == "function_call" {
            self.tool(format!("responses:{}", v["output_index"]));
        }
        if let Some(blocks) = root["content"].as_array() {
            for (i, b) in blocks.iter().enumerate() {
                if b["type"] == "tool_use" {
                    self.tool(format!("anthropic:{i}"));
                }
            }
        }
        if content && !self.json && self.metric.ttft_ms.is_none() {
            self.metric.ttft_ms = Some(elapsed);
        }
    }
    fn tool(&mut self, key: String) {
        if self.tools.len() < 1024 {
            self.tools.insert(key);
        }
        self.metric.tool_calls = self.tools.len() as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_sse_utf8_and_cumulative_usage() {
        let data = "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"test\",\"usage\":{\"input_tokens\":20,\"output_tokens\":1}}}\r\n\r\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"café\"}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":10}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":12}}\n\n";
        let mut o = Observer::new(RequestMetric::default(), "text/event-stream");
        for b in data.as_bytes() {
            o.feed(&[*b], 42.);
        }
        o.finish(100.);
        assert_eq!(o.metric.usage.input, Some(20));
        assert_eq!(o.metric.usage.output, Some(12));
        assert_eq!(o.metric.ttft_ms, Some(42.));
        assert_eq!(o.metric.parse_errors, 0);
    }
    #[test]
    fn ollama_final_without_newline() {
        let mut o = Observer::new(RequestMetric::default(), "application/x-ndjson");
        o.feed(
            br#"{"done":true,"eval_count":20,"eval_duration":1000000000,"prompt_eval_count":10}"#,
            1000.,
        );
        o.finish(1000.);
        assert_eq!(o.metric.generation_tps, Some(20.));
        assert_eq!(o.metric.ttft_ms, None);
    }
    #[test]
    fn oversize_recovers_and_unknown_is_not_zero() {
        let mut o = Observer::new(RequestMetric::default(), "text/event-stream");
        o.feed(&vec![b'x'; LIMIT + 10], 1.);
        o.feed(b"\n\ndata: {\"usage\":{\"completion_tokens\":3}}\n\n", 2.);
        assert_eq!(o.metric.parse_errors, 1);
        assert_eq!(o.metric.usage.output, Some(3));
        assert_eq!(o.metric.usage.input, None);
    }
    #[test]
    fn json_and_tool_fragments() {
        let mut o = Observer::new(RequestMetric::default(), "application/json");
        o.feed(
            b"{\n\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3}}",
            1.,
        );
        o.finish(2.);
        assert_eq!(o.metric.usage.input, Some(10));
        let mut o = Observer::new(RequestMetric::default(), "text/event-stream");
        for _ in 0..3 {
            o.feed(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0}]}}]}\n\n", 1.);
        }
        assert_eq!(o.metric.tool_calls, 1);
    }
}
