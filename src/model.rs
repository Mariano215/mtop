use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub type Shared = Arc<Mutex<Store>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RequestMetric {
    pub id: u64,
    pub provider: String,
    pub model: String,
    pub status: String,
    pub usage: Usage,
    pub ttft_ms: Option<f64>,
    pub duration_ms: Option<f64>,
    pub generation_tps: Option<f64>,
    pub tool_calls: u64,
    pub estimated_cost_usd: Option<f64>,
    pub parse_errors: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Backend {
    pub source: String,
    pub status: String,
    pub model: String,
    pub vram_bytes: Option<u64>,
    pub running: Option<f64>,
    pub waiting: Option<f64>,
    pub cache_fraction: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Price {
    pub model: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
}

impl Price {
    pub fn cost(&self, provider: &str, usage: &Usage) -> Option<f64> {
        let mut input = usage.input?;
        let output = usage.output?;
        let cached = usage.cache_read.unwrap_or(0);
        let written = usage.cache_write.unwrap_or(0);
        // OpenAI reports cached tokens as a subset; Anthropic reports them separately.
        if provider != "anthropic" {
            input = input.checked_sub(cached)?;
        }
        let read_rate = if cached > 0 {
            self.cache_read_per_million?
        } else {
            0.0
        };
        let write_rate = if written > 0 {
            self.cache_write_per_million?
        } else {
            0.0
        };
        Some(
            (input as f64 * self.input_per_million
                + output as f64 * self.output_per_million
                + cached as f64 * read_rate
                + written as f64 * write_rate)
                / 1_000_000.0,
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Store {
    pub requests: VecDeque<RequestMetric>,
    pub backends: Vec<Backend>,
    /// One "provider url -> listen address" line per running proxy listener.
    pub listeners: Vec<String>,
    /// One "tool name -> what MTop does about it" pair per tool found on this machine.
    pub sources: Vec<(String, String)>,
    /// Requests received over OpenTelemetry, per provider.
    pub telemetry: std::collections::BTreeMap<String, u64>,
    pub completed: u64,
    pub evicted: u64,
    pub known_cost_usd: f64,
    pub unpriced: u64,
    #[serde(skip)]
    capacity: usize,
    /// Set by --history. Sending never blocks on disk: one background thread writes.
    #[serde(skip)]
    pub history: Option<std::sync::mpsc::Sender<RequestMetric>>,
}

impl Store {
    pub fn shared(capacity: usize) -> Shared {
        Arc::new(Mutex::new(Self {
            requests: VecDeque::new(),
            backends: vec![],
            listeners: vec![],
            sources: vec![],
            telemetry: Default::default(),
            completed: 0,
            evicted: 0,
            known_cost_usd: 0.0,
            unpriced: 0,
            capacity: capacity.max(1),
            history: None,
        }))
    }
    pub fn update(&mut self, item: RequestMetric) {
        if let Some(old) = self.requests.iter_mut().find(|r| r.id == item.id) {
            *old = item;
        } else {
            if self.requests.len() == self.capacity {
                self.requests.pop_front();
                self.evicted += 1;
            }
            self.requests.push_back(item);
        }
    }
    pub fn finish(&mut self, item: RequestMetric) {
        self.completed += 1;
        if let Some(cost) = item.estimated_cost_usd {
            self.known_cost_usd += cost;
        } else {
            self.unpriced += 1;
        }
        if let Some(history) = &self.history {
            let _ = history.send(item.clone());
        }
        self.update(item);
    }
    /// Set the status line for one tool, replacing any earlier one by name.
    pub fn source(&mut self, name: &str, status: String) {
        match self.sources.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = status,
            None => self.sources.push((name.into(), status)),
        }
    }
    pub fn backend(&mut self, items: Vec<Backend>, source: &str) {
        self.backends.retain(|b| b.source != source);
        self.backends.extend(items.into_iter().take(64));
    }
}

pub fn safe_label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_accounting_and_missing_rates() {
        let p = Price {
            model: "x".into(),
            input_per_million: 2.,
            output_per_million: 4.,
            cache_read_per_million: Some(0.5),
            cache_write_per_million: None,
        };
        let u = Usage {
            input: Some(100),
            output: Some(10),
            cache_read: Some(40),
            cache_write: None,
        };
        assert!((p.cost("openai", &u).unwrap() - 0.00018).abs() < 1e-12);
        assert!((p.cost("anthropic", &u).unwrap() - 0.00026).abs() < 1e-12);
        assert!(
            p.cost(
                "openai",
                &Usage {
                    cache_write: Some(1),
                    ..u
                }
            )
            .is_none()
        );
    }
    #[test]
    fn retention_does_not_reset_totals() {
        let s = Store::shared(1);
        let mut s = s.lock().unwrap();
        for id in 0..3 {
            s.finish(RequestMetric {
                id,
                estimated_cost_usd: Some(1.),
                ..Default::default()
            });
        }
        assert_eq!(s.requests.len(), 1);
        assert_eq!(s.evicted, 2);
        assert_eq!(s.known_cost_usd, 3.);
    }
}
