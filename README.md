# MTop

A Rust terminal console for local model telemetry and opt-in API request observation.
**v0.1 is a tested starter implementation, not a universal passive AI monitor.**

## Start on your Mac

Install the stable Rust toolchain using [rustup](https://rustup.rs/) if needed, then:

```sh
git clone https://github.com/Mariano215/mtop.git
cd mtop
cargo test --locked
cargo run --locked -- --demo
cargo run --locked -- --once
cargo run --locked
```

The normal mode polls Ollama at `http://127.0.0.1:11434`. An unavailable service is displayed explicitly.
`--demo` uses synthetic data and makes no network calls. `--once` emits JSON and exits.

## Observe requests

Start MTop with one explicitly configured upstream origin. Keep `/v1` in the client URL, not the upstream origin:

```sh
# Observe Ollama requests, including its final token/timing statistics.
cargo run --locked -- --upstream http://127.0.0.1:11434 --provider ollama

# In another terminal, replace YOUR_INSTALLED_MODEL with an installed model name.
curl http://127.0.0.1:8088/api/generate -d '{"model":"YOUR_INSTALLED_MODEL","prompt":"Say hello"}'

# Observe an OpenAI-compatible provider. Configure your client's base URL
# to http://127.0.0.1:8088/v1 and keep its existing API-key configuration.
cargo run --locked -- --upstream https://api.openai.com --provider openai

# Anthropic Messages: configure a compatible client to use this proxy origin.
cargo run --locked -- --upstream https://api.anthropic.com --provider anthropic

# Optional vLLM server-level metrics.
cargo run --locked -- --vllm http://127.0.0.1:8000
```

Only clients routed through the proxy are observed. No request modification enables extra usage fields:
configure usage reporting in the client if supported. Missing usage is shown as unavailable.
API credentials pass through to the fixed upstream; MTop does not log or retain them in telemetry.
The listener is loopback-only. Do not expose it with port forwarding: it has no client authentication.

## Implemented

- Ratatui dashboard; selection, display freeze, clean terminal restoration, offline demo and JSON snapshot.
- Ollama loaded-model/VRAM polling; vLLM queue and cache gauges (maximum cache fraction across labels).
- Streaming proxy with unchanged response bytes, status and end-to-end headers; redirects disabled.
- Bounded SSE/NDJSON/JSON parsing for OpenAI Chat Completions, a subset of Responses events, Anthropic Messages and Ollama.
- Reported token/cache usage, first visible streamed-text latency, Ollama server generation speed, and tool-call counts.
- Optional exact-model pricing. Unknown usage or missing cache rates produce an unknown estimate, never a fabricated zero.
- 1,000 retained request rows by default; session totals survive row eviction.
- Linux/macOS CI workflow and protocol, state, UI and local mock-proxy tests.

## Explicit limits

No eBPF capture, PID attribution, full agent execution trees, tool arguments, prompt inspector, process termination,
Gemini-native parser, dedicated llama.cpp collector, cost-rate chart or automatic pricing catalog yet.
Responses normalization is partial; tool counts are capped at 1,024 unique calls per request.
TTFT means time from forwarding start to first visible text event. Nonstreaming TTFT is unavailable.
No tokenization estimates are made. Polling cannot recover per-request usage.

Request bodies are capped at 4 MiB; parser lines/events at 256 KiB; upstream concurrency at 16;
backend response bodies at 2 MiB; retained model names at 120 characters.
Oversized parser records increase `Parse`; forwarding continues. Large prompts exceeding the request limit receive HTTP 413.
Requests time out after 600 seconds (body intake: 30 seconds). Very long jobs need configurable limits in a later iteration.
Only numeric metadata and bounded model/provider/status labels remain in the store. Parsing transiently touches plaintext;
this is not secure memory erasure or protection from OS swap/core dumps. No persistence or analytics service is enabled.
Closing MTop also closes its active proxy connections.

## Prices

Use `--prices path/to/prices.json`. The file contains a JSON array:

```json
[
  {
    "model": "YOUR_EXACT_MODEL_ID",
    "input_per_million": 2.0,
    "output_per_million": 8.0,
    "cache_read_per_million": 0.2,
    "cache_write_per_million": 2.5
  }
]
```

These numbers are examples, not provider prices. Supply rates applicable to your account and model.
Estimates exclude taxes, service/tool charges, tiered context rates, special cache TTL pricing, discounts and subscriptions.
Anthropic input excludes separately reported cache tokens; OpenAI cached tokens are a subset of input tokens.

See [the refined specification](docs/SPEC.md) and [the Mac/Codex handoff](docs/HANDOFF.md).
