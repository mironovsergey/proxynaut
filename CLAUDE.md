# CLAUDE.md

Instructions for Claude Code agents working on this repository.

## Project

`proxynaut` is a local CLI daemon that exposes SOCKS5 and HTTP proxy endpoints
on `127.0.0.1` and multiplexes outgoing traffic across a pool of SSH tunnels,
with health checking, weighted balancing, and sticky-by-destination routing via
consistent hashing.

**Source of truth:** [`docs/SPEC.md`](docs/SPEC.md). When this file and SPEC.md
disagree, SPEC.md wins. Before working on any feature, read the relevant section
of SPEC.md.

## Workspace Layout

```
crates/
  proxynaut-core/   — library: config, ssh pool, balancer, health, proxy, control
  proxynaut-cli/    — binary: clap-based CLI, ipc client, daemon supervisor
docs/SPEC.md        — full specification (canonical)
examples/           — sample configs
.github/workflows/  — CI
```

Bin name is `proxynaut` (not `proxynaut-cli`).

## Tech Stack Snapshot

Async: `tokio`. SSH: `russh`, `russh-keys`. Balancer: `hashring`. Config:
`serde` + `toml`. CLI: `clap` (derive). Logging: `tracing` +
`tracing-subscriber`. IPC: `interprocess` (tokio integration). Paths:
`directories`. Errors: `thiserror` (in core), `anyhow` (in cli).

Versions are pinned in the workspace root `Cargo.toml` under
`[workspace.dependencies]`. Use `dep = { workspace = true }` in crate manifests
— never specify versions in crate-level `Cargo.toml`.

## Code Style

- File names: `snake_case.rs`. Modules: `snake_case`. Types: `PascalCase`.
  Functions and variables: `snake_case`. Constants: `SCREAMING_SNAKE_CASE`.
- Indentation, line length, and most formatting concerns: handled by `rustfmt`
  with defaults. Run `cargo fmt --all` before every commit.
- All `if`, `for`, `while`, `loop`, and `match` arms use explicit braces, even
  for single-expression bodies. `rustfmt` does not rewrite this — write it that
  way from the start. Example:

  ```rust
  // good
  match state {
      State::Connecting => { return Err(Error::NotReady); }
      State::Healthy => { handle_request().await }
      State::Dead => { return Err(Error::NoUpstreams); }
  }

  // bad
  match state {
      State::Connecting => return Err(Error::NotReady),
      State::Healthy => handle_request().await,
      State::Dead => return Err(Error::NoUpstreams),
  }
  ```

- Prefer clarity over cleverness. The author is learning Rust; favor explicit,
  slightly verbose code over idiomatic-but-dense alternatives. When in doubt,
  write the version that a reader unfamiliar with the codebase can follow line
  by line.
- Avoid chained method calls longer than 3 steps. Break into intermediate
  bindings with descriptive names.
- Prefer the standard library over crates when both are reasonable.
- All public items in `proxynaut-core` get doc comments. Private items get them
  when intent isn't obvious from the signature.

## Hard Constraints

These are non-negotiable. If a task seems to require violating one, stop and
ask.

- **No `unsafe`.** Anywhere. If a crate dependency forces it, document why and
  isolate it behind a safe wrapper.
- **No `unwrap()` or `expect()` in non-test code.** Propagate errors via
  `thiserror` (in `proxynaut-core`) or `anyhow` (in `proxynaut-cli`). In tests,
  both are acceptable.
- **No warnings.** `cargo clippy --workspace --all-targets` must be clean. If a
  lint is wrong for a specific case, use `#[allow(...)]` on the smallest
  possible scope with a comment explaining why.
- **No commits without `cargo fmt` and `cargo clippy` passing.**
- **No new dependencies without justification** in the commit message body.
  State the alternative considered and why this crate was chosen.
- **No `println!` / `eprintln!` for diagnostics.** Use `tracing` macros
  (`info!`, `debug!`, etc.). `println!` is reserved for CLI user-facing output
  (help text, status table).
- **No blocking I/O inside async functions.** File reads at startup (before the
  runtime starts) can be synchronous; everything else must use `tokio`
  equivalents.

## Working Approach

- **Balanced initiative.** Note observations as you work — dead code, duplicated
  logic, opportunities for simplification — but do not act on them in the same
  change. Surface them as a follow-up suggestion at the end of the response. The
  author decides what to address and when.
- **Refactoring suggestions are welcome.** When you write a verbose explicit
  version per the style rules, and an idiomatic Rust version would also work,
  mention the idiomatic alternative at the end with a brief explanation. The
  author may adopt it once familiar.
- **Stay scoped.** Implement what was asked. Don't refactor adjacent code,
  rename things, or "while we're here" anything without explicit permission.
- **Explain Rust concepts when introducing them.** First time `Arc<Mutex<T>>`
  appears in a session, briefly say what it does and why. Once seen, assume it's
  understood.
- **When uncertain about a design decision:** first check SPEC.md for an answer.
  If SPEC.md doesn't cover it, ask the author rather than inventing a
  convention.
- **When uncertain about a Rust API:** use the documentation, not guesses. If
  you're not sure a method exists or has the signature you think it does, say
  so.

## Testing

- Unit tests live in the same file as the code, in a
  `#[cfg(test)] mod tests { ... }` block at the bottom. Standard Rust
  convention.
- Integration tests for `proxynaut-core` live in `crates/proxynaut-core/tests/`.
  Tests that require a real SSH server use a dockerized `openssh-server` and are
  gated behind a feature flag or env var so `cargo test` without docker stays
  green.
- Write tests alongside the code in the same session, not as a separate pass.
- No coverage target. Test what gives confidence: pure functions (config
  parsing, balancer ring construction, hash key derivation), state machine
  transitions, error paths.
- `cargo test --workspace` must pass before any commit.

## Commits

- **Conventional Commits.** Types: `feat`, `fix`, `chore`, `docs`, `refactor`,
  `test`, `perf`, `build`, `ci`, `style`.
- Scope is optional; when used, it's a crate or module name (`feat(core): ...`,
  `fix(cli/status): ...`).
- Subject ≤ 72 chars, imperative mood, no trailing period.
- Body explains _why_, not _what_ (the diff shows what). Wrap at 72 chars.
- One logical change per commit. No mixing refactor with feature work.

Example:

```
feat(core): add consistent-hash balancer

Replace the placeholder round-robin selector with a hashring-based
balancer keyed by destination host. This implements sticky-by-destination
routing as specified in SPEC.md §3.4, preventing OAuth session
invalidation on services sensitive to source-IP changes.

Weights are honored via proportional virtual node counts (150 per unit).
```

## Communication Language

- **Code, comments, identifiers, log messages, error strings, doc comments,
  commit messages, file names, branch names, PR titles, GitHub issue text:**
  English.
- **Chat replies to the author:** Russian.
- **This file and SPEC.md:** English (canonical project documentation).

## Quick Commands

```bash
cargo fmt --all                              # before every commit
cargo clippy --workspace --all-targets       # must be clean
cargo test --workspace                       # must pass
cargo build --workspace                      # release: --release
cargo run -p proxynaut-cli -- <subcommand>   # run during development

cargo doc --workspace --no-deps --open       # browse docs locally
cargo tree -p proxynaut-cli                  # inspect dependency graph
```

## Out of Scope

See SPEC.md §12 for the full list. In short: not a VPN, not transparent
proxying, not multi-tenant, not protocol inspection. If a request seems to push
toward any of these, flag it and ask before proceeding.
