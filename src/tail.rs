//! Passive collectors: read what coding agents already write to disk.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<slug>/<session>.jsonl`; assistant lines carry the model
//! and the token counts the API reported, user lines carry tool results, and
//! every line carries the session, working directory and CLI version. Codex
//! appends to `~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`; `token_count`
//! events carry usage, rate limits and the context window, turn lines carry
//! the model and policies, and response items carry tool calls. Reading
//! these needs no proxy, no configuration and no root, so a plain `mtop` sees
//! sessions started in any other terminal.

use crate::model::{Limit, RequestMetric, Shared, Usage, safe_label};
use crate::scan::home;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

/// A transcript untouched for this long is history, not activity: skip its
/// existing contents and report only what is appended from now on.
const RECENT: Duration = Duration::from_secs(3600);
/// A transcript written to this recently counts as an active session.
const ACTIVE: Duration = Duration::from_secs(120);
/// One line of a transcript. Longer is truncated rather than buffered.
const MAX_LINE: usize = 256 * 1024;
/// Request ids kept for duplicate suppression.
const MAX_SEEN: usize = 100_000;
/// Directory depth walked under a source root.
const MAX_DEPTH: usize = 6;
/// Tool calls awaiting their result, per file.
const MAX_PENDING: usize = 1024;

/// One thing a transcript line said.
pub enum Event {
    Request(Box<RequestMetric>),
    Tool {
        name: String,
        duration_ms: Option<f64>,
        ok: bool,
    },
    Limit(Limit),
    Environment(&'static str, String),
}

/// One tool whose transcripts are readable.
pub struct Source {
    pub name: &'static str,
    /// Directory under the home directory.
    pub dir: &'static str,
    parse: fn(&str, &Path, &mut FileState) -> Vec<Event>,
}

pub const SOURCES: &[Source] = &[
    Source {
        name: "Claude Code",
        dir: ".claude/projects",
        parse: parse_claude,
    },
    Source {
        name: "Codex",
        dir: ".codex/sessions",
        parse: parse_codex,
    },
];

impl Source {
    pub fn path(&self) -> Option<PathBuf> {
        let dir = home()?.join(self.dir);
        dir.is_dir().then_some(dir)
    }
}

#[derive(Default)]
struct FileState {
    offset: u64,
    /// Codex names the model on a turn line, not on the usage line.
    model: String,
    session: String,
    project: String,
    tier: String,
    context_window: Option<u64>,
    /// Stamp of the last line that was input to the model (a user turn or a
    /// tool result). The next usage line's stamp minus this is the turn time.
    input_ms: Option<i64>,
    /// Tool calls by id: name and the stamp they were issued at.
    pending: HashMap<String, (String, Option<i64>)>,
}

/// Milliseconds since the Unix epoch for an RFC 3339 UTC stamp like
/// `2026-09-07T22:06:38.324Z`. Anything else is None. No calendar crate:
/// the civil-date arithmetic is a dozen lines and never wrong for UTC.
fn epoch_ms(stamp: &str) -> Option<i64> {
    let s = stamp.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, sec) = (t.next()??, t.next()??, t.next()??);
    let ms: i64 = format!("{:0<3}", frac.chars().take(3).collect::<String>())
        .parse()
        .ok()?;
    // Days from civil, Howard Hinnant's algorithm.
    let (y, m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((((days * 24 + h) * 60 + mi) * 60 + sec) * 1000 + ms)
}

/// Turn time for a usage line stamped `now`, measured from the last input line.
fn turn_ms(state: &FileState, now: Option<i64>) -> Option<f64> {
    let now = now?;
    let start = state.input_ms?;
    (now >= start).then_some((now - start) as f64)
}

fn elapsed(from: Option<i64>, to: Option<i64>) -> Option<f64> {
    let (a, b) = (from?, to?);
    (b >= a).then_some((b - a) as f64)
}

/// Last path component, so a project is named without exposing the whole path.
fn project_of(cwd: &str) -> String {
    safe_label(
        Path::new(cwd)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(cwd),
    )
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[derive(Default)]
pub struct Tailer {
    files: HashMap<PathBuf, FileState>,
    seen: HashSet<u64>,
}

impl Tailer {
    /// Read every transcript once, push whatever is new into the store, and
    /// return how many transcripts were written to in the last two minutes.
    pub fn poll(&mut self, source: &Source, dir: &Path, store: &Shared) -> usize {
        let mut active = 0;
        for path in transcripts(dir) {
            // First sight of a file is history, not activity: keep it out of rates.
            let backfill = !self.files.contains_key(&path);
            let (events, is_active) = self.read_new(source, &path);
            active += usize::from(is_active);
            if events.is_empty() {
                continue;
            }
            let mut s = store.lock().unwrap();
            for event in events {
                match event {
                    Event::Request(m) if backfill => s.finish_backfill(*m),
                    Event::Request(m) => s.finish(*m),
                    Event::Tool {
                        name,
                        duration_ms,
                        ok,
                    } => s.tool(&name, duration_ms, ok),
                    Event::Limit(l) => s.limit(l),
                    Event::Environment(k, v) => s.environment(k, &v),
                }
            }
        }
        active
    }

    fn read_new(&mut self, source: &Source, path: &Path) -> (Vec<Event>, bool) {
        let Ok(mut file) = File::open(path) else {
            return (vec![], false);
        };
        let Ok(meta) = file.metadata() else {
            return (vec![], false);
        };
        let len = meta.len();
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok());
        let active = age.is_some_and(|a| a < ACTIVE);
        let state = self.files.entry(path.into()).or_insert_with(|| FileState {
            // First sight of the file. An idle one starts at its end.
            offset: if age.is_some_and(|a| a < RECENT) {
                0
            } else {
                len
            },
            ..Default::default()
        });
        // Truncation or rotation: start over rather than read from the middle.
        if state.offset > len {
            state.offset = 0;
        }
        let start = state.offset;
        if file.seek(SeekFrom::Start(start)).is_err() {
            return (vec![], active);
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return (vec![], active);
        }
        // Lossy: a partial UTF-8 sequence at the tail must not drop the batch.
        let text = String::from_utf8_lossy(&bytes);

        let mut consumed = 0usize;
        let mut out = vec![];
        for line in text.split_inclusive('\n') {
            if !line.ends_with('\n') {
                break; // A line still being written. Re-read it next poll.
            }
            consumed += line.len();
            if line.len() > MAX_LINE {
                continue;
            }
            for event in (source.parse)(line, path, state) {
                if let Event::Request(m) = &event {
                    if self.seen.len() >= MAX_SEEN {
                        self.seen.clear();
                    }
                    if !self.seen.insert(m.id) {
                        continue;
                    }
                }
                out.push(event);
            }
        }
        state.offset = start + consumed as u64;
        (out, active)
    }
}

fn transcripts(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&path, depth + 1, out);
                }
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = vec![];
    walk(dir, 0, &mut out);
    out
}

/// The proxy numbers its requests from 1, so a hashed id must not land there.
/// The high bit marks a tailed request and keeps the two spaces apart.
fn id_for(key: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish() | (1 << 63)
}

fn remember_pending(state: &mut FileState, id: &str, name: &str, stamp: Option<i64>) {
    if state.pending.len() >= MAX_PENDING {
        state.pending.clear();
    }
    state.pending.insert(id.into(), (safe_label(name), stamp));
}

/// Every event on one Claude Code line.
fn parse_claude(line: &str, _: &Path, state: &mut FileState) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    let mut out = vec![];
    let stamp = v["timestamp"].as_str().and_then(epoch_ms);
    if let Some(id) = v["sessionId"].as_str() {
        state.session = short_id(id);
    }
    if let Some(cwd) = v["cwd"].as_str() {
        state.project = project_of(cwd);
    }
    if let Some(version) = v["version"].as_str() {
        out.push(Event::Environment(
            "Claude Code version (transcript)",
            version.into(),
        ));
    }
    let kind = v["type"].as_str().unwrap_or_default();
    let message = &v["message"];
    if kind == "user" {
        // A user turn or a tool result: the model starts on it next.
        if stamp.is_some() {
            state.input_ms = stamp;
        }
        if let Some(items) = message["content"].as_array() {
            for item in items.iter().filter(|c| c["type"] == "tool_result") {
                let id = item["tool_use_id"].as_str().unwrap_or_default();
                let (name, issued) = state
                    .pending
                    .remove(id)
                    .unwrap_or_else(|| ("unknown".into(), None));
                out.push(Event::Tool {
                    name,
                    duration_ms: elapsed(issued, stamp),
                    ok: !item["is_error"].as_bool().unwrap_or(false),
                });
            }
        }
        return out;
    }
    if kind != "assistant" {
        return out;
    }
    let mut tool_calls = 0u64;
    if let Some(items) = message["content"].as_array() {
        for item in items.iter().filter(|c| c["type"] == "tool_use") {
            tool_calls += 1;
            if let (Some(id), Some(name)) = (item["id"].as_str(), item["name"].as_str()) {
                remember_pending(state, id, name, stamp);
            }
        }
    }
    let request_id = v["requestId"].as_str().unwrap_or_default();
    let id = if request_id.is_empty() {
        match message["id"].as_str() {
            Some(mid) => id_for(&mid),
            None => return out,
        }
    } else {
        id_for(&request_id)
    };
    let usage = &message["usage"];
    let api_error = v["isApiErrorMessage"].as_bool().unwrap_or(false);
    let http_status = v["apiErrorStatus"].as_u64().map(|s| s.min(999) as u16);
    // No usage object means no numbers to report, unless the line is an error.
    let input = usage["input_tokens"].as_u64();
    if input.is_none() && !api_error {
        return out;
    }
    out.push(Event::Request(Box::new(RequestMetric {
        id,
        provider: "claude-code".into(),
        model: safe_label(message["model"].as_str().unwrap_or("unknown")),
        status: if api_error {
            "api error".into()
        } else {
            safe_label(message["stop_reason"].as_str().unwrap_or("logged"))
        },
        usage: Usage {
            input,
            output: usage["output_tokens"].as_u64(),
            cache_read: usage["cache_read_input_tokens"].as_u64(),
            cache_write: usage["cache_creation_input_tokens"].as_u64(),
        },
        reasoning: usage["output_tokens_details"]["thinking_tokens"]
            .as_u64()
            .filter(|n| *n > 0),
        tool_calls: tool_calls.min(1024),
        duration_ms: turn_ms(state, stamp),
        http_status,
        error: if api_error {
            safe_label(v["error"].as_str().unwrap_or("api error"))
        } else {
            String::new()
        },
        source: if v["isSidechain"].as_bool().unwrap_or(false) {
            "subagent".into()
        } else {
            "main".into()
        },
        session: state.session.clone(),
        project: state.project.clone(),
        ..Default::default()
    })));
    out
}

/// Every event on one Codex line.
fn parse_codex(line: &str, path: &Path, state: &mut FileState) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    let mut out = vec![];
    let payload = &v["payload"];
    let stamp = v["timestamp"].as_str().and_then(epoch_ms);
    let line_kind = v["type"].as_str().unwrap_or_default();
    if let Some(model) = payload["model"].as_str() {
        state.model = safe_label(model);
    }
    if let Some(cwd) = payload["cwd"].as_str() {
        state.project = project_of(cwd);
    }
    match line_kind {
        "session_meta" => {
            if let Some(id) = payload["id"].as_str() {
                state.session = short_id(id);
            }
            if let Some(ver) = payload["cli_version"].as_str() {
                out.push(Event::Environment("Codex version (transcript)", ver.into()));
            }
            return out;
        }
        "turn_context" => {
            state.input_ms = stamp.or(state.input_ms);
            if let Some(effort) = payload["effort"].as_str() {
                state.tier = safe_label(effort);
            }
            for (key, label) in [
                ("approval_policy", "Codex approval policy"),
                ("sandbox_policy", "Codex sandbox"),
            ] {
                let value = payload[key]
                    .as_str()
                    .or_else(|| payload[key]["type"].as_str());
                if let Some(value) = value {
                    out.push(Event::Environment(label, value.into()));
                }
            }
            return out;
        }
        _ => (),
    }
    let kind = payload["type"].as_str().unwrap_or_default();
    match kind {
        "user_message" => {
            state.input_ms = stamp.or(state.input_ms);
            return out;
        }
        "function_call" | "custom_tool_call" => {
            if let (Some(id), Some(name)) = (payload["call_id"].as_str(), payload["name"].as_str())
            {
                remember_pending(state, id, name, stamp);
            }
            return out;
        }
        "function_call_output" | "custom_tool_call_output" => {
            state.input_ms = stamp.or(state.input_ms);
            let id = payload["call_id"].as_str().unwrap_or_default();
            let (name, issued) = state
                .pending
                .remove(id)
                .unwrap_or_else(|| ("unknown".into(), None));
            out.push(Event::Tool {
                name,
                duration_ms: elapsed(issued, stamp),
                ok: true,
            });
            return out;
        }
        "token_count" => (),
        _ => return out,
    }
    // Rate limits ride on every token_count line, even ones with no usage.
    for name in ["primary", "secondary"] {
        let l = &payload["rate_limits"][name];
        if let Some(used) = l["used_percent"].as_f64() {
            out.push(Event::Limit(Limit {
                provider: "codex".into(),
                name: name.into(),
                used_percent: used,
                window_minutes: l["window_minutes"].as_u64(),
                resets_at: l["resets_at"].as_i64(),
                plan: safe_label(payload["rate_limits"]["plan_type"].as_str().unwrap_or("")),
            }));
        }
    }
    let info = &payload["info"];
    if let Some(window) = info["model_context_window"].as_u64() {
        state.context_window = Some(window);
    }
    let last = &info["last_token_usage"];
    let Some(input) = last["input_tokens"].as_u64() else {
        return out;
    };
    // Codex reports cached tokens as a subset of input, like the OpenAI API.
    out.push(Event::Request(Box::new(RequestMetric {
        id: id_for(&(path, v["timestamp"].as_str().unwrap_or_default())),
        provider: "codex".into(),
        model: if state.model.is_empty() {
            "unknown".into()
        } else {
            state.model.clone()
        },
        status: "logged".into(),
        usage: Usage {
            input: Some(input),
            output: last["output_tokens"].as_u64(),
            cache_read: last["cached_input_tokens"].as_u64(),
            cache_write: None,
        },
        reasoning: last["reasoning_output_tokens"].as_u64().filter(|n| *n > 0),
        context_window: state.context_window,
        duration_ms: turn_ms(state, stamp),
        source: "main".into(),
        session: state.session.clone(),
        project: state.project.clone(),
        tier: state.tier.clone(),
        ..Default::default()
    })));
    out
}

/// One pass over every source whose directory exists, updating its status line.
pub fn poll_all(tailer: &mut Tailer, store: &Shared) {
    for (source, dir) in SOURCES.iter().filter_map(|s| s.path().map(|p| (s, p))) {
        let active = tailer.poll(source, &dir, store);
        store.lock().unwrap().source(
            source.name,
            format!("watching ~/{}, {active} active", source.dir),
        );
    }
}

/// Tail every source whose directory exists, once a second, forever.
pub async fn run(store: Shared) {
    let mut tailer = Tailer::default();
    loop {
        let mut t = std::mem::take(&mut tailer);
        let s = store.clone();
        // Directory walks and reads are blocking I/O; keep them off the runtime.
        if let Ok(t) = tokio::task::spawn_blocking(move || {
            poll_all(&mut t, &s);
            t
        })
        .await
        {
            tailer = t;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const LINE: &str = r#"{"type":"assistant","requestId":"req_A","sessionId":"abcdef12-3456","cwd":"/home/u/proj","version":"2.1.263","isSidechain":false,"timestamp":"2026-01-01T00:00:02.250Z","message":{"model":"claude-fable-5-1","id":"msg_1","stop_reason":"tool_use","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"Bash"}],"usage":{"input_tokens":32,"cache_creation_input_tokens":1228,"cache_read_input_tokens":60875,"output_tokens":208,"output_tokens_details":{"thinking_tokens":40}}}}"#;

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
    fn reads_claude_usage_context_and_tools() {
        let mut st = FileState::default();
        let p = Path::new("x");
        let user =
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00.000Z","message":{"content":"go"}}"#;
        assert!(requests(parse_claude(user, p, &mut st)).is_empty());
        let m = requests(parse_claude(LINE, p, &mut st)).remove(0);
        assert_eq!(m.model, "claude-fable-5-1");
        assert_eq!(m.usage.input, Some(32));
        assert_eq!(m.usage.output, Some(208));
        assert_eq!(m.usage.cache_read, Some(60875));
        assert_eq!(m.usage.cache_write, Some(1228));
        assert_eq!(m.reasoning, Some(40));
        assert_eq!(m.tool_calls, 1);
        assert_eq!(m.duration_ms, Some(2250.));
        assert_eq!(m.session, "abcdef12");
        assert_eq!(m.project, "proj");
        assert_eq!(m.source, "main");
        assert!(m.id & (1 << 63) != 0, "must not collide with proxy ids");
        // The tool result closes the call with its duration and outcome.
        let result = r#"{"type":"user","timestamp":"2026-01-01T00:00:05.250Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true}]}}"#;
        let events = parse_claude(result, p, &mut st);
        assert!(matches!(
            events.last(),
            Some(Event::Tool { name, duration_ms: Some(ms), ok: false }) if name == "Bash" && *ms == 3000.
        ));
        // An API error line is a request with a status and no usage.
        let err = r#"{"type":"assistant","requestId":"req_E","isApiErrorMessage":true,"apiErrorStatus":429,"error":"rate limited","message":{"model":"claude-fable-5-1","content":[]}}"#;
        let m = requests(parse_claude(err, p, &mut st)).remove(0);
        assert_eq!(m.http_status, Some(429));
        assert_eq!(m.status, "api error");
        assert!(requests(parse_claude("not json", p, &mut st)).is_empty());
    }

    #[test]
    fn epoch_ms_parses_utc_stamps_only() {
        assert_eq!(epoch_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(
            epoch_ms("2026-09-07T22:06:38.324Z"),
            Some(1_788_818_798_324)
        );
        assert!(epoch_ms("2026-09-07T22:06:38+02:00").is_none());
        assert!(epoch_ms("t1").is_none());
    }

    #[test]
    fn reads_codex_usage_limits_context_and_tools() {
        let mut st = FileState::default();
        let p = Path::new("rollout.jsonl");
        let meta = r#"{"type":"session_meta","payload":{"id":"01a07c6d-f05b","cwd":"/w/repo","cli_version":"0.153.4"}}"#;
        let ev = parse_codex(meta, p, &mut st);
        assert!(matches!(ev.first(), Some(Event::Environment(_, v)) if v == "0.153.4"));
        let turn = r#"{"timestamp":"2026-01-01T00:00:00.000Z","type":"turn_context","payload":{"model":"gpt-5.5","effort":"high","approval_policy":"never","sandbox_policy":{"type":"read-only"}}}"#;
        assert_eq!(parse_codex(turn, p, &mut st).len(), 2);
        let call = r#"{"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1"}}"#;
        assert!(parse_codex(call, p, &mut st).is_empty());
        let done = r#"{"timestamp":"2026-01-01T00:00:03.500Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1"}}"#;
        assert!(matches!(
            parse_codex(done, p, &mut st).first(),
            Some(Event::Tool { name, duration_ms: Some(ms), ok: true }) if name == "exec_command" && *ms == 2500.
        ));
        let empty = r#"{"timestamp":"t0","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"primary":{"used_percent":3.0,"window_minutes":300,"resets_at":1},"plan_type":"plus"}}}"#;
        let ev = parse_codex(empty, p, &mut st);
        assert_eq!(ev.len(), 1, "a limit but no request");
        assert!(
            matches!(ev.first(), Some(Event::Limit(l)) if l.used_percent == 3.0 && l.plan == "plus")
        );
        let line = r#"{"timestamp":"2026-01-01T00:00:06.000Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400,"last_token_usage":{"input_tokens":19627,"cached_input_tokens":2432,"output_tokens":341,"reasoning_output_tokens":19}}}}"#;
        let m = requests(parse_codex(line, p, &mut st)).remove(0);
        assert_eq!(m.model, "gpt-5.5");
        assert_eq!(m.usage.input, Some(19627));
        assert_eq!(m.reasoning, Some(19));
        assert_eq!(m.context_window, Some(258400));
        assert_eq!(m.tier, "high");
        assert_eq!(m.project, "repo");
        assert_eq!(m.session, "01a07c6d");
        assert_eq!(
            m.duration_ms,
            Some(2500.),
            "from the tool result to the usage line"
        );
    }

    #[test]
    fn tails_appends_once_and_holds_partial_lines() {
        let dir = std::env::temp_dir().join(format!("mtop-tail-{}", std::process::id()));
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("s.jsonl");
        std::fs::write(&path, format!("{LINE}\n")).unwrap();

        let store = crate::model::Store::shared(10);
        let mut t = Tailer::default();
        let src = &SOURCES[0];
        assert_eq!(t.poll(src, &dir, &store), 1, "a fresh file is active");
        assert_eq!(store.lock().unwrap().completed, 1);
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 1);

        let mut f = File::options().append(true).open(&path).unwrap();
        let second = LINE.replace("req_A", "req_B");
        write!(f, "{}", &second[..20]).unwrap();
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 1);
        writeln!(f, "{}", &second[20..]).unwrap();
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 2);

        writeln!(f, "{second}").unwrap();
        t.poll(src, &dir, &store);
        assert_eq!(store.lock().unwrap().completed, 2);
        assert_eq!(store.lock().unwrap().by_project["proj"].requests, 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
