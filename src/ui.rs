use crate::model::{RequestMetric, Shared, Store};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Row, Table, TableState},
};
use std::time::Duration;

/// One request-table column: header, width, and how a row fills it.
type Cell = Box<dyn Fn(&RequestMetric) -> String>;
type Column = (&'static str, Constraint, Cell);

fn number(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
}
fn decimal(n: Option<f64>) -> String {
    n.map(|n| format!("{n:.1}")).unwrap_or_else(|| "—".into())
}

pub fn draw(f: &mut Frame, s: &Store, selected: usize, paused: bool, demo: bool) {
    // One extra line per proxy listener, so the ports stay on screen while you configure a client.
    let listener_lines = s.listeners.len().min(4) as u16;
    let source_lines = s.sources.len().min(8) as u16 + u16::from(!s.telemetry.is_empty());
    let areas = Layout::vertical([
        Constraint::Length(3 + listener_lines + source_lines),
        Constraint::Length(7),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(f.area());
    let title = format!(
        " MTop 0.1 • {}{} ",
        if demo {
            "DEMO / SYNTHETIC"
        } else {
            "LIVE / METRICS ONLY"
        },
        if paused { " • FROZEN" } else { "" }
    );
    // A zero nobody measured is not a number; say unavailable instead.
    let nothing_priced = s.completed > 0 && s.unpriced == s.completed;
    let mut lines: Vec<Line> = vec![Line::from(vec![
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
    ])];
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
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().title(title))
            .style(Style::default().fg(Color::Yellow)),
        areas[0],
    );
    // An empty box must say why, like every other box on the screen.
    let mut backends: Vec<Row> = if s.backends.is_empty() {
        vec![Row::new(vec![
            "none".to_string(),
            "not polled".to_string(),
            "no local server polled: drop --no-ollama, or pass --vllm <URL>".to_string(),
        ])]
    } else {
        vec![]
    };
    backends.extend(s.backends.iter().map(|b| {
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
            backends,
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
            Block::bordered().title(" Backends • polling does not observe individual requests "),
        ),
        areas[1],
    );
    // A column that is "—" on every row is not information. Columns whose
    // source only some providers report (TTFT, Parse) or that need a price
    // table (Est. USD) appear only once some row can fill them.
    let has_ttft = s.requests.iter().any(|r| r.ttft_ms.is_some());
    let has_cost = s.requests.iter().any(|r| r.estimated_cost_usd.is_some());
    let has_parse = s.requests.iter().any(|r| r.parse_errors > 0);
    let mut columns: Vec<Column> = vec![
        (
            "Provider",
            Constraint::Length(12),
            Box::new(|r| r.provider.clone()),
        ),
        // Sized to the longest model actually present, like btop, so the
        // box has no dead width. Min 15 keeps the header readable when empty.
        (
            "Model",
            Constraint::Length(
                s.requests
                    .iter()
                    .map(|r| r.model.chars().count())
                    .max()
                    .unwrap_or(0)
                    .clamp(15, 40) as u16,
            ),
            Box::new(|r| r.model.clone()),
        ),
        (
            "Status",
            Constraint::Length(10),
            Box::new(|r| r.status.clone()),
        ),
    ];
    if has_ttft {
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
            Box::new(|r: &RequestMetric| decimal(r.duration_ms))
                as Box<dyn Fn(&RequestMetric) -> String>,
        ),
        (
            "Input",
            Constraint::Length(8),
            Box::new(|r| number(r.usage.input)),
        ),
        (
            "Cache",
            Constraint::Length(8),
            Box::new(|r| {
                number(match (r.usage.cache_read, r.usage.cache_write) {
                    (None, None) => None,
                    (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
                })
            }),
        ),
        (
            "Output",
            Constraint::Length(8),
            Box::new(|r| number(r.usage.output)),
        ),
        (
            "Tools",
            Constraint::Length(5),
            Box::new(|r| r.tool_calls.to_string()),
        ),
    ]);
    if has_cost {
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
    if has_parse {
        columns.push((
            "Parse",
            Constraint::Length(5),
            Box::new(|r| r.parse_errors.to_string()),
        ));
    }
    let rows: Vec<Row> = s
        .requests
        .iter()
        .rev()
        .map(|r| {
            Row::new(
                columns
                    .iter()
                    .map(|(_, _, cell)| cell(r))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let mut state = TableState::default().with_selected(Some(selected));
    f.render_stateful_widget(
        Table::new(rows, columns.iter().map(|(_, w, _)| *w).collect::<Vec<_>>())
            .header(
                Row::new(columns.iter().map(|(name, _, _)| *name).collect::<Vec<_>>())
                    .style(Style::default().fg(Color::Cyan)),
            )
            .row_highlight_style(Style::default().bg(Color::DarkGray))
            .block(Block::bordered().title(
                " Observed requests • Cache = tokens read from or written to prompt cache • — means unavailable ",
            )),
        areas[2],
        &mut state,
    );
    f.render_widget(Paragraph::new("↑/↓ or j/k select • Space freeze display • q/Esc quit • No prompts, credentials or tool arguments retained")
        .block(Block::bordered()), areas[3]);
}

pub fn run(store: Shared, demo: bool) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| -> anyhow::Result<()> {
        let mut paused = false;
        let mut selected = 0usize;
        let mut snapshot = store.lock().unwrap().clone();
        loop {
            if !paused {
                snapshot = store.lock().unwrap().clone();
            }
            selected = selected.min(snapshot.requests.len().saturating_sub(1));
            terminal.draw(|f| draw(f, &snapshot, selected, paused, demo))?;
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Char(' ') => paused = !paused,
                    KeyCode::Down | KeyCode::Char('j') => selected = selected.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
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
    #[test]
    fn renders_small_and_standard_terminals() {
        for (w, h) in [(40, 10), (120, 30)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            let s = Store::shared(10);
            t.draw(|f| draw(f, &s.lock().unwrap(), 0, false, true))
                .unwrap();
        }
    }
}
