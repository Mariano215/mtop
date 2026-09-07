use crate::model::{RequestMetric, Shared, Store, Totals};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Row, Table, TableState},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One request-table column: header, width, and how a row fills it.
type Cell = Box<dyn Fn(&RequestMetric) -> String>;
type Column = (&'static str, Constraint, Cell);

fn number(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
}
fn decimal(n: Option<f64>) -> String {
    n.map(|n| format!("{n:.1}")).unwrap_or_else(|| "—".into())
}
fn percent(n: Option<f64>) -> String {
    n.map(|n| format!("{:.0}%", n * 100.))
        .unwrap_or_else(|| "—".into())
}
fn money(n: f64) -> String {
    format!("${n:.4}")
}
fn hms(seconds: f64) -> String {
    let s = seconds.max(0.) as u64;
    format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
}

/// Row order for the request table. `s` cycles it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// Arrival order, newest first: the live-log view.
    #[default]
    Newest,
    /// Longest turn first: what is slow right now.
    Slowest,
    /// Most tokens first: what is expensive right now.
    Biggest,
}

impl Sort {
    fn next(self) -> Self {
        match self {
            Sort::Newest => Sort::Slowest,
            Sort::Slowest => Sort::Biggest,
            Sort::Biggest => Sort::Newest,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Sort::Newest => "newest",
            Sort::Slowest => "slowest",
            Sort::Biggest => "biggest",
        }
    }
}

/// What the middle box shows. Tab cycles it; Enter opens the selected request.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    #[default]
    Backends,
    Tools,
    Models,
    Projects,
    Sessions,
    Environment,
    Detail,
}

impl Panel {
    fn next(self) -> Self {
        match self {
            Panel::Backends => Panel::Tools,
            Panel::Tools => Panel::Models,
            Panel::Models => Panel::Projects,
            Panel::Projects => Panel::Sessions,
            Panel::Sessions => Panel::Environment,
            Panel::Environment | Panel::Detail => Panel::Backends,
        }
    }
}

/// Everything the screen needs besides the store.
#[derive(Clone, Copy, Default)]
pub struct View {
    pub selected: usize,
    pub paused: bool,
    pub demo: bool,
    pub sort: Sort,
    pub panel: Panel,
}

/// Requests in the order the table shows them.
fn ordered(s: &Store, sort: Sort) -> Vec<&RequestMetric> {
    let mut rows: Vec<&RequestMetric> = s.requests.iter().rev().collect();
    match sort {
        Sort::Newest => (),
        Sort::Slowest => rows.sort_by(|a, b| {
            b.duration_ms
                .unwrap_or(-1.)
                .total_cmp(&a.duration_ms.unwrap_or(-1.))
        }),
        Sort::Biggest => rows.sort_by_key(|r| std::cmp::Reverse(r.total_tokens())),
    }
    rows
}

pub fn draw(f: &mut Frame, s: &Store, v: View) {
    let (tokens_per_min, cost_per_hour) = s.rates();
    // One line per proxy listener, source, limit and the rate line.
    let header_lines = 2
        + s.listeners.len().min(4) as u16
        + s.sources.len().min(8) as u16
        + s.limits.len().min(4) as u16
        + u16::from(!s.telemetry.is_empty());
    let areas = Layout::vertical([
        Constraint::Length(2 + header_lines),
        Constraint::Length(9),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(f.area());

    // Header.
    let title = format!(
        " MTop 0.1 • {}{} ",
        if v.demo {
            "DEMO / SYNTHETIC"
        } else {
            "LIVE / METRICS ONLY"
        },
        if v.paused { " • FROZEN" } else { "" }
    );
    // A zero nobody measured is not a number; say unavailable instead.
    let nothing_priced = s.completed > 0 && s.unpriced == s.completed;
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw(format!(
                "Completed {}  |  Priced estimate {}  |  Unpriced {}{}  |  ",
                s.completed,
                if nothing_priced {
                    "—".to_string()
                } else {
                    format!("${:.6}", s.known_cost_usd)
                },
                s.unpriced,
                if nothing_priced {
                    " (no --prices given)"
                } else {
                    ""
                },
            )),
            // Dropped rows are the one number here that means data was lost.
            Span::styled(
                format!("Evicted {}", s.evicted),
                Style::default().fg(if s.evicted > 0 {
                    Color::Red
                } else {
                    Color::Yellow
                }),
            ),
        ]),
        Line::raw(format!(
            "Last 5 min: {:.0} tokens/min  |  {}/hour  |  {} tool calls  |  {} models  |  {} sessions",
            tokens_per_min,
            if nothing_priced {
                "— ".to_string()
            } else {
                money(cost_per_hour)
            },
            s.tools.values().map(|t| t.calls).sum::<u64>(),
            s.by_model.len(),
            s.by_session.len(),
        )),
    ];
    for line in s.listeners.iter().take(4) {
        lines.push(Line::raw(line.clone()));
    }
    // Color says the state, so the first frame reads at a glance:
    // green is being watched, yellow needs a step, gray cannot be observed.
    for (name, status) in s.sources.iter().take(8) {
        let color = if status.starts_with("watching") {
            Color::Green
        } else if status.contains("not observable") {
            Color::DarkGray
        } else {
            Color::Yellow
        };
        lines.push(Line::styled(
            format!("{name:<12} {status}"),
            Style::default().fg(color),
        ));
    }
    if !s.telemetry.is_empty() {
        let counts: Vec<String> = s
            .telemetry
            .iter()
            .map(|(p, n)| format!("{p} {n}"))
            .collect();
        lines.push(Line::styled(
            format!("{:<12} {}", "telemetry", counts.join(", ")),
            Style::default().fg(Color::Green),
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    for l in s.limits.iter().take(4) {
        let window = l
            .window_minutes
            .map(|m| format!(" of {}", hms(m as f64 * 60.)))
            .unwrap_or_default();
        let resets = l
            .resets_at
            .map(|t| format!(", resets in {}", hms((t - now) as f64)))
            .unwrap_or_default();
        let plan = if l.plan.is_empty() {
            String::new()
        } else {
            format!(" ({})", l.plan)
        };
        // Past 80 percent the next call may be the one that is refused.
        let color = if l.used_percent >= 80. {
            Color::Red
        } else if l.used_percent >= 50. {
            Color::Yellow
        } else {
            Color::Green
        };
        lines.push(Line::styled(
            format!(
                "{:<12} {} limit {:.0}% used{window}{resets}{plan}",
                l.provider, l.name, l.used_percent
            ),
            Style::default().fg(color),
        ));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(title))
            .style(Style::default().fg(Color::Yellow)),
        areas[0],
    );

    // Middle box: one panel at a time.
    let rows = ordered(s, v.sort);
    match v.panel {
        Panel::Backends => backends(f, s, areas[1]),
        Panel::Tools => tools(f, s, areas[1]),
        Panel::Models => totals(f, &s.by_model, "Model", " Totals by model ", areas[1]),
        Panel::Projects => totals(
            f,
            &s.by_project,
            "Project",
            " Totals by project (working directory name) ",
            areas[1],
        ),
        Panel::Sessions => totals(f, &s.by_session, "Session", " Totals by session ", areas[1]),
        Panel::Environment => environment(f, s, areas[1]),
        Panel::Detail => detail(f, rows.get(v.selected).copied(), areas[1]),
    }

    // Request table. A column that is "—" on every row is not information:
    // columns only some providers report appear once some row can fill them.
    let any = |pred: &dyn Fn(&RequestMetric) -> bool| s.requests.iter().any(pred);
    let model_width = s
        .requests
        .iter()
        .map(|r| r.model.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(15, 24) as u16;
    let mut columns: Vec<Column> = vec![
        (
            "Provider",
            Constraint::Length(11),
            Box::new(|r| r.provider.clone()),
        ),
        (
            "Model",
            Constraint::Length(model_width),
            Box::new(|r| r.model.clone()),
        ),
        (
            "Status",
            Constraint::Length(9),
            Box::new(|r| r.status.clone()),
        ),
    ];
    if any(&|r| r.source == "subagent" || r.source == "auxiliary") {
        columns.push((
            "Src",
            Constraint::Length(4),
            Box::new(|r| r.source.chars().take(4).collect()),
        ));
    }
    if any(&|r| !r.agent.is_empty()) {
        columns.push((
            "Agent",
            Constraint::Length(10),
            Box::new(|r| r.agent.clone()),
        ));
    }
    if any(&|r| !r.tier.is_empty()) {
        columns.push(("Tier", Constraint::Length(8), Box::new(|r| r.tier.clone())));
    }
    if any(&|r| r.ttft_ms.is_some()) {
        columns.push((
            "TTFT ms",
            Constraint::Length(8),
            Box::new(|r| decimal(r.ttft_ms)),
        ));
    }
    columns.extend([
        (
            "Dur ms",
            Constraint::Length(8),
            Box::new(|r: &RequestMetric| decimal(r.duration_ms)) as Cell,
        ),
        (
            "Input",
            Constraint::Length(7),
            Box::new(|r| number(r.usage.input)),
        ),
        (
            "Cache",
            Constraint::Length(7),
            Box::new(|r| {
                number(match (r.usage.cache_read, r.usage.cache_write) {
                    (None, None) => None,
                    (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
                })
            }),
        ),
    ]);
    if any(&|r| r.cache_hit().is_some()) {
        columns.push((
            "Hit",
            Constraint::Length(4),
            Box::new(|r| percent(r.cache_hit())),
        ));
    }
    if any(&|r| r.context_fill().is_some()) {
        columns.push((
            "Ctx",
            Constraint::Length(4),
            Box::new(|r| percent(r.context_fill())),
        ));
    }
    columns.push((
        "Output",
        Constraint::Length(7),
        Box::new(|r| number(r.usage.output)),
    ));
    if any(&|r| r.reasoning.is_some()) {
        columns.push((
            "Reason",
            Constraint::Length(7),
            Box::new(|r| number(r.reasoning)),
        ));
    }
    columns.push((
        "Tools",
        Constraint::Length(5),
        Box::new(|r| r.tool_calls.to_string()),
    ));
    if any(&|r| r.http_status.is_some()) {
        columns.push((
            "HTTP",
            Constraint::Length(4),
            Box::new(|r| {
                r.http_status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".into())
            }),
        ));
    }
    if any(&|r| r.estimated_cost_usd.is_some()) {
        columns.push((
            "Est. USD",
            Constraint::Length(9),
            Box::new(|r| {
                r.estimated_cost_usd
                    .map(|v| format!("{v:.6}"))
                    .unwrap_or_else(|| "—".into())
            }),
        ));
    }
    if any(&|r| r.parse_errors > 0) {
        columns.push((
            "Parse",
            Constraint::Length(5),
            Box::new(|r| r.parse_errors.to_string()),
        ));
    }
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|r| {
            let style = if !r.error.is_empty() || r.http_status.is_some_and(|s| s >= 400) {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Row::new(
                columns
                    .iter()
                    .map(|(_, _, cell)| cell(r))
                    .collect::<Vec<_>>(),
            )
            .style(style)
        })
        .collect();
    let mut state = TableState::default().with_selected(Some(v.selected));
    f.render_stateful_widget(
        Table::new(
            table_rows,
            columns.iter().map(|(_, w, _)| *w).collect::<Vec<_>>(),
        )
        .header(
            Row::new(columns.iter().map(|(name, _, _)| *name).collect::<Vec<_>>())
                .style(Style::default().fg(Color::Cyan)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .block(Block::bordered().title(format!(
            " Observed requests • sort: {} • Hit = cache read share • Ctx = context used • — means unavailable ",
            v.sort.label()
        ))),
        areas[2],
        &mut state,
    );
    f.render_widget(
        Paragraph::new(
            "↑/↓ j/k select • Enter detail • Tab panel: backends, tools, models, projects, sessions, environment • s sort • Space freeze • q quit",
        )
        .block(Block::bordered()),
        areas[3],
    );
}

fn backends(f: &mut Frame, s: &Store, area: ratatui::layout::Rect) {
    // An empty box must say why, like every other box on the screen.
    let mut rows: Vec<Row> = if s.backends.is_empty() {
        vec![Row::new(vec![
            "none".to_string(),
            "not polled".to_string(),
            "no local server polled: drop --no-ollama, or pass --vllm <URL>".to_string(),
        ])]
    } else {
        vec![]
    };
    rows.extend(s.backends.iter().map(|b| {
        Row::new(vec![
            b.source.clone(),
            b.status.clone(),
            b.model.clone(),
            b.vram_bytes
                .map(|v| format!("{:.2} GiB", v as f64 / 1073741824.))
                .unwrap_or_else(|| "—".into()),
            decimal(b.running),
            decimal(b.waiting),
            b.cache_fraction
                .map(|v| format!("{:.1}%", v * 100.))
                .unwrap_or_else(|| "—".into()),
        ])
    }));
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(9),
                Constraint::Length(13),
                Constraint::Min(15),
                Constraint::Length(12),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Length(8),
            ],
        )
        .header(
            Row::new([
                "Source", "Status", "Model", "VRAM", "Running", "Waiting", "KV max",
            ])
            .style(Style::default().fg(Color::Cyan)),
        )
        .block(
            Block::bordered()
                .title(" Backends • polling does not observe individual requests • Tab for more "),
        ),
        area,
    );
}

fn tools(f: &mut Frame, s: &Store, area: ratatui::layout::Rect) {
    let mut entries: Vec<_> = s.tools.iter().collect();
    entries.sort_by_key(|(_, t)| std::cmp::Reverse(t.calls));
    let mut rows: Vec<Row> = entries
        .iter()
        .map(|(name, t)| {
            let avg = (t.timed > 0).then(|| t.total_ms / t.timed as f64);
            Row::new(vec![
                (*name).clone(),
                t.calls.to_string(),
                t.failures.to_string(),
                decimal(avg),
                format!("{:.1}", t.total_ms / 1000.),
            ])
            .style(if t.failures > 0 {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            })
        })
        .collect();
    if rows.is_empty() {
        rows.push(Row::new(vec![
            "none yet".to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ]));
    }
    let decisions: Vec<String> = s
        .counters
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("claude_code.tool_decision.")
                .map(|d| format!("{d} {v:.0}"))
        })
        .collect();
    let title = if decisions.is_empty() {
        " Tools • calls, failures, average and total time ".to_string()
    } else {
        format!(" Tools • decisions: {} ", decisions.join(", "))
    };
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Min(16),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Length(9),
                Constraint::Length(9),
            ],
        )
        .header(
            Row::new(["Tool", "Calls", "Failed", "Avg ms", "Total s"])
                .style(Style::default().fg(Color::Cyan)),
        )
        .block(Block::bordered().title(title)),
        area,
    );
}

fn totals(
    f: &mut Frame,
    map: &std::collections::BTreeMap<String, Totals>,
    key: &str,
    title: &str,
    area: ratatui::layout::Rect,
) {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by_key(|(_, t)| std::cmp::Reverse(t.input + t.cache + t.output));
    let mut rows: Vec<Row> = entries
        .iter()
        .map(|(name, t)| {
            Row::new(vec![
                (*name).clone(),
                t.requests.to_string(),
                t.input.to_string(),
                t.cache.to_string(),
                t.output.to_string(),
                t.reasoning.to_string(),
                if t.unpriced == t.requests {
                    "—".into()
                } else {
                    money(t.cost_usd)
                },
                t.errors.to_string(),
            ])
            .style(if t.errors > 0 {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            })
        })
        .collect();
    if rows.is_empty() {
        rows.push(Row::new(vec!["none yet".to_string()]));
    }
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Min(16),
                Constraint::Length(6),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(6),
            ],
        )
        .header(
            Row::new([
                key, "Reqs", "Input", "Cache", "Output", "Reason", "Cost", "Errors",
            ])
            .style(Style::default().fg(Color::Cyan)),
        )
        .block(Block::bordered().title(title.to_string())),
        area,
    );
}

fn environment(f: &mut Frame, s: &Store, area: ratatui::layout::Rect) {
    let mut lines: Vec<Line> = s
        .environment
        .iter()
        .map(|(k, v)| Line::raw(format!("{k:<28} {v}")))
        .collect();
    for (k, v) in &s.counters {
        if k.starts_with("claude_code.tool_decision.") {
            continue;
        }
        let shown = match k.as_str() {
            "claude_code.active_time.total" => format!("active time {}", hms(*v)),
            _ => format!("{k} {v:.0}"),
        };
        lines.push(Line::raw(format!("{:<28} {}", "counter", shown)));
    }
    if lines.is_empty() {
        lines.push(Line::raw("nothing found yet"));
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" Environment • versions, policies, telemetry state, exported counters "),
        ),
        area,
    );
}

fn detail(f: &mut Frame, r: Option<&RequestMetric>, area: ratatui::layout::Rect) {
    let lines: Vec<Line> = match r {
        None => vec![Line::raw("no request selected")],
        Some(r) => {
            let pairs: Vec<(&str, String)> = vec![
                ("provider / model", format!("{} / {}", r.provider, r.model)),
                (
                    "status / http / attempt",
                    format!(
                        "{} / {} / {}",
                        r.status,
                        r.http_status
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "—".into()),
                        number(r.attempt)
                    ),
                ),
                (
                    "tokens in / cache read / cache write / out / reasoning",
                    format!(
                        "{} / {} / {} / {} / {}",
                        number(r.usage.input),
                        number(r.usage.cache_read),
                        number(r.usage.cache_write),
                        number(r.usage.output),
                        number(r.reasoning)
                    ),
                ),
                (
                    "cache hit / context used / window",
                    format!(
                        "{} / {} / {}",
                        percent(r.cache_hit()),
                        percent(r.context_fill()),
                        number(r.context_window)
                    ),
                ),
                (
                    "ttft / duration / tokens per s",
                    format!(
                        "{} ms / {} ms / {}",
                        decimal(r.ttft_ms),
                        decimal(r.duration_ms),
                        decimal(r.generation_tps)
                    ),
                ),
                (
                    "session / project / source / agent / tier",
                    format!(
                        "{} / {} / {} / {} / {}",
                        r.session, r.project, r.source, r.agent, r.tier
                    )
                    .replace("//", "— /"),
                ),
                (
                    "tool calls / est. USD / parse errors",
                    format!(
                        "{} / {} / {}",
                        r.tool_calls,
                        r.estimated_cost_usd
                            .map(|v| format!("{v:.6}"))
                            .unwrap_or_else(|| "—".into()),
                        r.parse_errors
                    ),
                ),
            ];
            let mut lines: Vec<Line> = pairs
                .into_iter()
                .map(|(k, v)| Line::raw(format!("{k:<48} {v}")))
                .collect();
            if !r.error.is_empty() {
                lines.push(Line::styled(
                    format!("{:<48} {}", "error", r.error),
                    Style::default().fg(Color::Red),
                ));
            }
            lines
        }
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(" Request detail • Tab or Esc to close ")),
        area,
    );
}

pub fn run(store: Shared, demo: bool) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| -> anyhow::Result<()> {
        let mut v = View {
            demo,
            ..Default::default()
        };
        let mut snapshot = store.lock().unwrap().clone();
        loop {
            if !v.paused {
                snapshot = store.lock().unwrap().clone();
            }
            v.selected = v.selected.min(snapshot.requests.len().saturating_sub(1));
            terminal.draw(|f| draw(f, &snapshot, v))?;
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Esc if v.panel == Panel::Detail => v.panel = Panel::Backends,
                    KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Char(' ') => v.paused = !v.paused,
                    KeyCode::Char('s') => v.sort = v.sort.next(),
                    KeyCode::Tab => v.panel = v.panel.next(),
                    KeyCode::Enter => {
                        v.panel = if v.panel == Panel::Detail {
                            Panel::Backends
                        } else {
                            Panel::Detail
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => v.selected = v.selected.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => v.selected = v.selected.saturating_sub(1),
                    _ => (),
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Limit, Usage};

    #[test]
    fn renders_every_panel_and_sort_on_small_and_standard_terminals() {
        let s = Store::shared(10);
        {
            let mut s = s.lock().unwrap();
            s.finish(RequestMetric {
                id: 1,
                provider: "codex".into(),
                model: "gpt-5.5".into(),
                usage: Usage {
                    input: Some(10),
                    output: Some(5),
                    cache_read: Some(30),
                    cache_write: None,
                },
                context_window: Some(100),
                reasoning: Some(2),
                http_status: Some(429),
                error: "limit".into(),
                session: "abc".into(),
                project: "p".into(),
                agent: "tdd".into(),
                tier: "high".into(),
                ..Default::default()
            });
            s.tool("Bash", Some(3.), false);
            s.limit(Limit {
                provider: "codex".into(),
                name: "primary".into(),
                used_percent: 85.,
                window_minutes: Some(300),
                resets_at: Some(0),
                plan: "plus".into(),
            });
            s.environment("Codex version", "0.1");
            s.count("claude_code.active_time.total", 90.);
            s.count("claude_code.tool_decision.accept", 1.);
        }
        for (w, h) in [(40, 10), (120, 34)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            for panel in [
                Panel::Backends,
                Panel::Tools,
                Panel::Models,
                Panel::Projects,
                Panel::Sessions,
                Panel::Environment,
                Panel::Detail,
            ] {
                for sort in [Sort::Newest, Sort::Slowest, Sort::Biggest] {
                    let v = View {
                        panel,
                        sort,
                        demo: true,
                        ..Default::default()
                    };
                    t.draw(|f| draw(f, &s.lock().unwrap(), v)).unwrap();
                }
            }
        }
    }
}
