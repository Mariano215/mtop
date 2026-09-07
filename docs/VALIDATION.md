# Validation record

Validated in the ChatGPT Linux x86_64 environment with Rust 1.98.1.

- `cargo test --locked -j 2`: 12 tests passed (8 unit/render tests and 4 mock HTTP integration tests).
- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed after fixing two collapsible-if findings.
- `cargo run --locked -- --demo --once`: runnable synthetic JSON snapshot.
- Interactive PTY demo: rendered successfully and exited with q, restoring terminal state.

Tests cover byte-fragmented UTF-8 SSE, CRLF framing, final NDJSON, cumulative Anthropic usage,
tool-fragment deduplication, oversized parser recovery, unavailable values, cache pricing,
bounded retention with cumulative totals, Prometheus aggregation, and small/standard TUI layouts.
HTTP tests exercise real loopback sockets with a mock upstream and verify streamed body/auth/header/status
preservation, HTTP errors, disabled redirect following, and the incoming request-size limit.

The first parallel dependency build encountered a transient archive/mapping error; retrying with two jobs succeeded.
There is no assertion that production performance budgets have been met.

Not yet validated: live cloud providers, the user's Ollama installation, macOS/Apple Silicon, CI execution,
release-profile resource budgets, complete cancellation/concurrency stress behavior, or Linux eBPF capture.
No real provider API charges or model downloads were incurred during these checks.

Publishing was attempted, but the GitHub integration returned HTTP 403, "Resource not accessible by integration"
for branch creation. The project is packaged for a local Git push; no remote branch or pull request was created.
