use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How far back rates look.
const RATE_WINDOW: Duration = Duration::from_secs(300);

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
    /// Reasoning or thinking tokens, when the tool reports them apart.
    pub reasoning: Option<u64>,
    /// The model's context window, when the tool reports it. With input and
    /// cache this gives how full the context is.
    pub context_window: Option<u64>,
    pub http_status: Option<u16>,
    pub attempt: Option<u64>,
    /// Bounded error text from the tool, empty when none.
    pub error: String,
    /// "main", "subagent" or "auxiliary", when known.
    pub source: String,
    /// Session id, shortened. Empty when unknown.
    pub session: String,
    /// Working directory's last path component. Empty when unknown.
    pub project: String,
    /// Agent, skill or plugin name on the call. Empty when none.
    pub agent: String,
    /// Speed, effort or service tier label. Empty when none.
    pub tier: String,
}

impl RequestMetric {
    pub fn total_tokens(&self) -> u64 {
        [
            self.usage.input,
            self.usage.output,
            self.usage.cache_read,
            self.usage.cache_write,
        ]
        .iter()
        .flatten()
        .sum()
    }
    /// Cache read as a share of everything the model read.
    pub fn cache_hit(&self) -> Option<f64> {
        let read = self.usage.cache_read?;
        let seen = read + self.usage.input? + self.usage.cache_write.unwrap_or(0);
        (seen > 0).then(|| read as f64 / seen as f64)
    }
    /// Context fill: what the model read this call over its window.
    pub fn context_fill(&self) -> Option<f64> {
        let window = self.context_window?;
        let seen = self.usage.input?
            + self.usage.cache_read.unwrap_or(0)
            + self.usage.cache_write.unwrap_or(0);
        (window > 0).then(|| seen as f64 / window as f64)
    }
}

/// Running totals for one key: a model, a project or a session.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Totals {
    pub requests: u64,
    pub input: u64,
    pub cache: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cost_usd: f64,
    pub unpriced: u64,
    pub errors: u64,
}

impl Totals {
    fn add(&mut self, m: &RequestMetric) {
        self.requests += 1;
        self.input += m.usage.input.unwrap_or(0);
        self.cache += m.usage.cache_read.unwrap_or(0) + m.usage.cache_write.unwrap_or(0);
        self.output += m.usage.output.unwrap_or(0);
        self.reasoning += m.reasoning.unwrap_or(0);
        match m.estimated_cost_usd {
            Some(c) => self.cost_usd += c,
            None => self.unpriced += 1,
        }
        if !m.error.is_empty() || m.http_status.is_some_and(|s| s >= 400) {
            self.errors += 1;
        }
    }
}

/// One tool's record: calls, failures and time spent.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolStat {
    pub calls: u64,
    pub failures: u64,
    pub total_ms: f64,
    pub timed: u64,
}

/// A usage quota the tool reported, like Codex's rate limit windows.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Limit {
    pub provider: String,
    pub name: String,
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    /// Unix seconds when the window resets.
    pub resets_at: Option<i64>,
    pub plan: String,
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
    pub telemetry: BTreeMap<String, u64>,
    pub by_model: BTreeMap<String, Totals>,
    pub by_project: BTreeMap<String, Totals>,
    pub by_session: BTreeMap<String, Totals>,
    pub tools: BTreeMap<String, ToolStat>,
    pub limits: Vec<Limit>,
    /// Versions, policies and settings found on this machine, as label pairs.
    pub environment: Vec<(String, String)>,
    /// Counters the tools export: active time, lines changed, commits, tool decisions.
    pub counters: BTreeMap<String, f64>,
    /// Completed requests in the last few minutes, for rates.
    #[serde(skip)]
    recent: VecDeque<(Instant, u64, f64)>,
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
            by_model: Default::default(),
            by_project: Default::default(),
            by_session: Default::default(),
            tools: Default::default(),
            limits: vec![],
            environment: vec![],
            counters: Default::default(),
            recent: VecDeque::new(),
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
    /// Record a request that happened before MTop started (a transcript
    /// backfill). Totals count it; the rate window does not.
    pub fn finish_backfill(&mut self, item: RequestMetric) {
        self.finish(item);
        self.recent.pop_back();
    }
    pub fn finish(&mut self, item: RequestMetric) {
        self.completed += 1;
        if let Some(cost) = item.estimated_cost_usd {
            self.known_cost_usd += cost;
        } else {
            self.unpriced += 1;
        }
        self.by_model
            .entry(item.model.clone())
            .or_default()
            .add(&item);
        if !item.project.is_empty() {
            self.by_project
                .entry(item.project.clone())
                .or_default()
                .add(&item);
        }
        if !item.session.is_empty() {
            self.by_session
                .entry(item.session.clone())
                .or_default()
                .add(&item);
        }
        self.recent.push_back((
            Instant::now(),
            item.total_tokens(),
            item.estimated_cost_usd.unwrap_or(0.),
        ));
        while self.recent.len() > 10_000 {
            self.recent.pop_front();
        }
        if let Some(history) = &self.history {
            let _ = history.send(item.clone());
        }
        self.update(item);
    }
    /// Tokens per minute and dollars per hour over the last five minutes.
    pub fn rates(&self) -> (f64, f64) {
        let since = Instant::now().checked_sub(RATE_WINDOW);
        let (mut tokens, mut cost) = (0u64, 0f64);
        for (at, t, c) in self.recent.iter().rev() {
            if since.is_some_and(|s| *at < s) {
                break;
            }
            tokens += t;
            cost += c;
        }
        let minutes = RATE_WINDOW.as_secs_f64() / 60.;
        (tokens as f64 / minutes, cost / minutes * 60.)
    }
    pub fn tool(&mut self, name: &str, duration_ms: Option<f64>, ok: bool) {
        if self.tools.len() >= 256 && !self.tools.contains_key(name) {
            return;
        }
        let t = self.tools.entry(safe_label(name)).or_default();
        t.calls += 1;
        if !ok {
            t.failures += 1;
        }
        if let Some(ms) = duration_ms {
            t.total_ms += ms;
            t.timed += 1;
        }
    }
    /// Replace the limit with the same provider and name, or add it.
    pub fn limit(&mut self, limit: Limit) {
        if let Some(l) = self
            .limits
            .iter_mut()
            .find(|l| l.provider == limit.provider && l.name == limit.name)
        {
            *l = limit;
        } else if self.limits.len() < 32 {
            self.limits.push(limit);
        }
    }
    pub fn environment(&mut self, key: &str, value: &str) {
        let value = safe_label(value);
        if let Some(e) = self.environment.iter_mut().find(|(k, _)| k == key) {
            e.1 = value;
        } else if self.environment.len() < 64 {
            self.environment.push((key.into(), value));
        }
    }
    pub fn count(&mut self, key: &str, by: f64) {
        if self.counters.len() >= 64 && !self.counters.contains_key(key) {
            return;
        }
        *self.counters.entry(safe_label(key)).or_default() += by;
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
    fn ratios_and_totals() {
        let m = RequestMetric {
            usage: Usage {
                input: Some(100),
                output: Some(10),
                cache_read: Some(300),
                cache_write: Some(0),
            },
            context_window: Some(800),
            model: "m".into(),
            project: "p".into(),
            session: "s".into(),
            ..Default::default()
        };
        assert_eq!(m.cache_hit(), Some(0.75));
        assert_eq!(m.context_fill(), Some(0.5));
        assert_eq!(m.total_tokens(), 410);
        let s = Store::shared(10);
        let mut s = s.lock().unwrap();
        s.finish(m.clone());
        s.finish(m);
        assert_eq!(s.by_model["m"].requests, 2);
        assert_eq!(s.by_project["p"].cache, 600);
        assert_eq!(s.by_session["s"].unpriced, 2);
        assert!(s.rates().0 > 0.);
        s.tool("Bash", Some(10.), true);
        s.tool("Bash", None, false);
        assert_eq!(
            (
                s.tools["Bash"].calls,
                s.tools["Bash"].failures,
                s.tools["Bash"].timed
            ),
            (2, 1, 1)
        );
        s.limit(Limit {
            provider: "codex".into(),
            name: "primary".into(),
            used_percent: 1.,
            ..Default::default()
        });
        s.limit(Limit {
            provider: "codex".into(),
            name: "primary".into(),
            used_percent: 2.,
            ..Default::default()
        });
        assert_eq!(s.limits.len(), 1);
        assert_eq!(s.limits[0].used_percent, 2.);
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
