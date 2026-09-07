use crate::model::{Shared, Store};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Paragraph, Row, Table, TableState},
};
use std::time::Duration;

fn number(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
}
fn decimal(n: Option<f64>) -> String {
    n.map(|n| format!("{n:.1}")).unwrap_or_else(|| "—".into())
}

pub fn draw(f: &mut Frame, s: &Store, selected: usize, paused: bool, demo: bool) {
    let areas = Layout::vertical([
        Constraint::Length(3),
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
    let summary = format!(
        "Completed {}  |  Priced estimate ${:.6}  |  Unpriced {}  |  Evicted {}",
        s.completed, s.known_cost_usd, s.unpriced, s.evicted
    );
    f.render_widget(
        Paragraph::new(summary)
            .block(Block::bordered().title(title))
            .style(Style::default().fg(Color::Yellow)),
        areas[0],
    );
    let backends: Vec<Row> = s
        .backends
        .iter()
        .map(|b| {
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
        })
        .collect();
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
    let rows: Vec<Row> = s
        .requests
        .iter()
        .rev()
        .map(|r| {
            Row::new(vec![
                r.id.to_string(),
                r.provider.clone(),
                r.model.clone(),
                r.status.clone(),
                decimal(r.ttft_ms),
                number(r.usage.input),
                number(r.usage.output),
                r.tool_calls.to_string(),
                r.estimated_cost_usd
                    .map(|v| format!("{v:.6}"))
                    .unwrap_or_else(|| "—".into()),
                r.parse_errors.to_string(),
            ])
        })
        .collect();
    let mut state = TableState::default().with_selected(Some(selected));
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(5),
                Constraint::Length(10),
                Constraint::Min(15),
                Constraint::Length(14),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(6),
            ],
        )
        .header(
            Row::new([
                "ID", "Provider", "Model", "Status", "TTFT ms", "Input", "Output", "Tools",
                "Est. USD", "Parse",
            ])
            .style(Style::default().fg(Color::Cyan)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .block(Block::bordered().title(" Observed requests • — means unavailable ")),
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
