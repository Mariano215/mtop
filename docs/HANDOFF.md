# Continue in Codex CLI on your Mac

The GitHub integration could read the repository but could not create a branch (HTTP 403).
The handoff ZIP contains the full source tree and `mtop.bundle` with a local implementation commit.
After extracting the ZIP, run from the extracted `mtop-handoff` folder:

```sh
git clone mtop.bundle mtop-work
cd mtop-work
git remote set-url origin https://github.com/Mariano215/mtop.git
git push -u origin codex/rust-mvp
```

Authenticate Git on your Mac through your normal GitHub setup if prompted.
This pushes a review branch; create or merge its pull request when you are ready.
The separate `source` folder is a plain source copy if you prefer to copy files into an existing checkout.

Run `cargo test --locked` and `cargo run --locked -- --demo` before testing real services.
Run Codex from the repository directory. Paste this prompt:

> Continue the MTop Rust implementation. Read README.md, docs/SPEC.md and docs/VALIDATION.md first.
> Preserve the metrics-only defaults and unknown-versus-zero accounting. Verify the locked build and TUI on this Mac.
> Inspect my local Ollama availability, then test polling and a streamed request through the loopback proxy using an already
> installed model. Do not download models or spend money on provider API calls without discussing it with me.
> Validate client disconnects, upstream errors, timeouts and concurrency limits. Fix concrete failures and add focused tests.
> Next, complete provider normalization and design explicit agent trace ingestion; do not claim universal passive capture.
> Keep docs and implementation status aligned. Linux eBPF work belongs on a Linux host or VM, not directly on macOS.

The original Google Doc is the design discussion source. docs/SPEC.md is the implementation-aligned specification.
