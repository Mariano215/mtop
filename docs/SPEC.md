# MTop system design specification

## 1. System overview and core objectives

MTop is a Rust terminal observability application. v0.1 runs without privileged capture on macOS and Linux.
Its ingestion modes are independent: polling for backend health, transcripts the coding agents already write,
each tool's own OpenTelemetry export, and an explicitly configured streaming HTTP reverse proxy. Universal
zero-configuration network capture is a research objective, not a product guarantee.

Rust provides memory-safe parsing and shared state, Tokio handles concurrent network I/O, Axum serves the proxy,
Reqwest/Rustls forwards HTTP requests, and Ratatui/Crossterm renders the interface. Cargo.lock fixes resolved dependencies.

The first deliverable is a useful, runnable vertical slice. Passive Linux capture, full agent traces, richer economics,
and broader provider support are subsequent milestones with their own acceptance criteria.

## 2. Architecture and ingestion

The proxy and backend pollers normalize observations into an in-memory Store. The UI reads snapshots at 10 Hz.
Proxy response chunks flow to the caller after bounded observation; the parser never changes response bytes.
There is no raw-payload event queue. A short-held mutex serializes store updates; this design does not claim lock-free execution.

| Ingestion | Implemented data | Boundary |
|---|---|---|
| Ollama `/api/ps` | Loaded model names, `size_vram` | No per-request token usage or queue inference |
| vLLM `/metrics` | Running/waiting gauges, cache fraction | Aggregate running/waiting across labels; maximum cache fraction |
| Explicit HTTP proxy | Request ID, model, streamed usage, visible-text timing, tool-call count | Only traffic routed through this listener; no PID or session inference |
| Transcript tail (`~/.claude/projects`, `~/.codex/sessions`) | Model, token usage, tool count, turn time from line stamps | Read-only; no prompt text retained; files idle over an hour start at their end |
| OTLP/HTTP JSON receiver (`mtop setup`) | `claude_code.api_request`, `codex.sse_event`, `gemini_cli.api_response`: model, tokens, duration, cost when the tool sends it | Loopback only; only numeric fields and the model name are read; metrics and traces discarded |
| Future Linux eBPF collector | Candidate TLS/HTTP observations | Separate privileged helper, supported-runtime matrix, loss accounting and explicit capture scope required |

The proxy fixes a single upstream origin and binds only to loopback. Authentication headers are forwarded.
Hop-by-hop headers and connection-nominated headers are removed. Redirects are not followed.
The request path/query and body are preserved; `Accept-Encoding: identity` simplifies telemetry observation.
This is not an HTTP CONNECT proxy. HTTP/2 multiplexing is handled by the HTTP libraries, not TCP-tuple session heuristics.

Future eBPF work must account for TLS-library and ABI differences, static binaries, Go runtime changes, read return lengths,
socket correlation, TLS implementations outside OpenSSL, HTTP/2 stream IDs, and capture truncation.
Do not expose a userspace payload pointer as though it were captured bytes. macOS is not a Linux eBPF target.

## 3. Telemetry and parsing

SSE supports CRLF, fragmented byte boundaries, multiline data fields and final pending data. NDJSON supports a final
record without newline. JSON responses are buffered only to the parser limit. Invalid and oversized records increment
an observable counter. Recovery continues at record boundaries; actual response forwarding is independent of parse success.

Implemented formats: OpenAI Chat Completions usage/deltas, selected Responses usage and text/tool events,
Anthropic message_start/message_delta/content_block events, and Ollama generate/chat usage/timings.
Gemini-native and fuller Responses/Ollama tool normalization remain planned.

| Metric | Definition | Unknown/error behavior |
|---|---|---|
| Input/output tokens | Latest provider-reported cumulative value | Optional, never defaulted to zero; Anthropic deltas replace rather than sum |
| Cached tokens | Provider field, separate read/write categories | Optional; cost unavailable when applicable cache rate missing |
| TTFT | Forwarding start to first visible streamed text event | Includes proxy/network overhead; unavailable for nonstreaming and tool-only output |
| Duration | Forwarding start to body completion/error/cancellation | Observed client-facing duration |
| Generation speed | Ollama eval_count / eval_duration in seconds | Not inferred from byte counts or chunk counts |
| Cost | User-supplied exact-model rates × reported usage | Labelled estimate; missing information remains unknown |
| Tool count | Distinct observed tool positions | Maximum 1,024; no names, arguments, results, exit codes or hidden reasoning |
| Context pressure | Future configured context-window calculation | Not implemented; do not imply that API input tokens alone prove actual window occupancy |

Each request gets a monotonic local ID. A TCP connection or PID is not an agent session ID.
Future trace ingestion should use explicit session/span/parent/tool-call identifiers from supported agent events or OpenTelemetry.
Only provider-returned text is observable; hidden model reasoning cannot be reconstructed.

## 4. TUI layout and controls

The header shows completed count, priced estimate subtotal, unpriced count and evictions. Backend rows display availability,
loaded models/VRAM or aggregate queue/cache gauges. Request rows show ID, provider, model, status, TTFT, usage, tool count,
estimated cost and parse failures. Unknown fields use an em dash. Demo mode is visibly synthetic.

Up/down or j/k selects; space freezes the view while collection continues; q, Escape or Ctrl-C exits.
Process kill, raw-prompt inspection and a redaction toggle are excluded from v0.1.
This removes the original conflicting `k` binding and avoids claiming process control without verified process identity.

## 5. Pipeline and state management

Defaults: 1,000 retained request rows (configurable 1–10,000), 16 concurrent upstream requests, 4 MiB request body,
256 KiB parser record/event, 2 MiB backend response, 64 models per backend, 120 characters per model label,
1,024 tool positions per request. These are component bounds, not a proven 40 MiB process RSS ceiling.

Completed totals and known cost survive row eviction. Active requests can be evicted and reappear on subsequent updates;
the UI is a bounded recent-observation view, not durable history. Finish guards account for completed, failed and cancelled streams.
Provider errors inside SSE remain errors even when HTTP status is 200. JSON snapshots intentionally go to stdout only when requested.

## 6. Performance and verification

Original <1.5% single-core CPU and <40 MiB RSS goals remain aspirational until measured under a specified workload.
Remove the unconditional zero-loss-at-100-MB/s claim. Proxy parsing failures and row evictions are distinct counters;
future eBPF capture must additionally report lost/truncated kernel events. No throughput claim follows from a ring-buffer size.
UI currently refreshes at 10 Hz, so <16 ms input-to-draw is not a guarantee.

Acceptance for this slice: reproducible locked build; fragmented protocol and cumulative usage tests; unknown-value/cache-price
tests; retention tests; small/standard terminal render tests; and a local mock upstream proving response byte/header/status
preservation and telemetry extraction. Real provider traffic, Apple Silicon runtime behavior, performance budgets and Linux capture
require environment-specific validation. CI targets Linux and macOS; configuring a workflow is not evidence that both have passed.

## 7. Security, privacy and sandboxing

No root required for v0.1. Metrics-only is the default and only inspection mode. Plaintext must transiently pass through the proxy
and parser; the Store retains numeric metadata and bounded labels, not prompts, headers or tool payloads.
No logs, database, analytics or external reporting channel is configured. OS swap and core dumps are outside this guarantee.
The unauthenticated loopback listener assumes trusted local users and must not be externally forwarded.

Shutdown terminates in-flight proxy connections. Upstream requests have a 600-second timeout, with a 30-second incoming-body
deadline and a 5-second upstream connect timeout. Remote use should employ HTTPS; local HTTP is useful for inference servers.
Future payload inspection requires capture scoping and redaction before retention, not a UI-only hide toggle.

## 8. Roadmap

1. Delivered: pollers, proxy, transcript tail, OTLP receiver, `setup`, history, five-target releases and installers.
2. Provider fixtures for OpenAI Responses and tool events; explicit trace ingestion with parent and span IDs.
3. A bundled price table with a documented source and date, so cost is known without `--prices`.
4. Passive Linux capture behind a separate optional collector, for one documented TLS and runtime combination.

## Technical references

- [Ratatui](https://docs.rs/ratatui/0.29.0/ratatui/)
- [Axum](https://docs.rs/axum/0.8.9/axum/)
- [Ollama usage](https://docs.ollama.com/api/usage)
- [vLLM metrics](https://docs.vllm.ai/en/latest/design/metrics/)
- [Anthropic streaming and cumulative usage](https://platform.claude.com/docs/en/build-with-claude/streaming)
- [Linux BPF ring buffers](https://docs.kernel.org/bpf/ringbuf.html)
