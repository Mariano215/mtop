use crate::model::{Backend, Shared, safe_label};
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::Value;
use std::time::Duration;

pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()?)
}

pub async fn bounded_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut stream = response.error_for_status()?.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len() + chunk.len() > limit {
            bail!("response exceeds configured limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub async fn once(client: &reqwest::Client, source: &str, base: &str) -> Result<Vec<Backend>> {
    let path = if source == "ollama" {
        "api/ps"
    } else {
        "metrics"
    };
    let response = client
        .get(format!("{}/{path}", base.trim_end_matches('/')))
        .timeout(Duration::from_secs(3))
        .send()
        .await?;
    let bytes = bounded_body(response, 2 * 1024 * 1024).await?;
    if source == "ollama" {
        let v: Value = serde_json::from_slice(&bytes)?;
        let models = v["models"].as_array().context("missing models array")?;
        if models.is_empty() {
            return Ok(vec![Backend {
                source: source.into(),
                status: "idle".into(),
                ..Default::default()
            }]);
        }
        Ok(models
            .iter()
            .take(64)
            .map(|m| Backend {
                source: source.into(),
                status: "online".into(),
                model: safe_label(m["name"].as_str().unwrap_or("unknown")),
                vram_bytes: m["size_vram"].as_u64(),
                ..Default::default()
            })
            .collect())
    } else {
        Ok(vec![parse_prometheus(std::str::from_utf8(&bytes)?)])
    }
}

pub fn parse_prometheus(text: &str) -> Backend {
    let mut b = Backend {
        source: "vllm".into(),
        status: "online".into(),
        model: "server aggregate".into(),
        ..Default::default()
    };
    let mut cache = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        // Labels may contain spaces. Find the end of the label set before reading the sample.
        let split = line
            .find('}')
            .map(|i| i + 1)
            .or_else(|| line.find(char::is_whitespace));
        let Some(i) = split else {
            continue;
        };
        let name = line[..i].split('{').next().unwrap_or("");
        let Some(value) = line[i..]
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<f64>().ok())
            .filter(|n| n.is_finite())
        else {
            continue;
        };
        match name {
            "vllm:num_requests_running" => b.running = Some(b.running.unwrap_or(0.) + value),
            "vllm:num_requests_waiting" => b.waiting = Some(b.waiting.unwrap_or(0.) + value),
            "vllm:kv_cache_usage_perc" | "vllm:gpu_cache_usage_perc" => cache.push(value),
            _ => (),
        }
    }
    // Maximum across label sets, not an invalid sum of percentages.
    b.cache_fraction = cache.into_iter().reduce(f64::max);
    b
}

pub async fn run(store: Shared, source: &'static str, base: String) {
    let Ok(client) = client() else {
        return;
    };
    loop {
        let rows = once(&client, source, &base).await.unwrap_or_else(|_| {
            vec![Backend {
                source: source.into(),
                status: "unavailable".into(),
                ..Default::default()
            }]
        });
        store.lock().unwrap().backend(rows, source);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregates_workers_and_skips_nan() {
        let b = parse_prometheus(
            "# HELP x\nvllm:num_requests_running{model_name=\"a b\"} 2\nvllm:num_requests_running{engine=\"1\"} 3\nvllm:kv_cache_usage_perc{engine=\"0\"} 0.2\nvllm:kv_cache_usage_perc{engine=\"1\"} 0.8\nvllm:num_requests_waiting NaN\n",
        );
        assert_eq!(b.running, Some(5.));
        assert_eq!(b.cache_fraction, Some(0.8));
        assert_eq!(b.waiting, None);
    }
}
