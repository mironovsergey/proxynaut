# Proxynaut — Technical Specification

> Status: draft v0.1 · Last updated: 2026-05-15 · Owner: @mironovsergey

## 1. Overview

### 1.1 What is Proxynaut

Proxynaut is a local CLI daemon that exposes a SOCKS5 and an HTTP proxy endpoint
on `127.0.0.1`, and multiplexes outgoing traffic across a pool of SSH tunnels to
remote VPS hosts. It performs health-checking, weighted balancing with
sticky-by-destination routing via consistent hashing, and is managed through a
single command-line interface.

The intended use case is bypassing network restrictions while preserving session
affinity for stateful HTTPS services (OAuth-protected APIs, web apps sensitive
to source-IP changes).

### 1.2 Primary Goals

- Single self-contained binary replacing the ad-hoc combination of `ssh -D` +
  `privoxy` for the author's day-to-day workflow.
- Stable destination-to-upstream affinity to avoid session invalidation on
  services like Claude Code, GitHub, etc.
- Transparent failover: if an upstream dies, traffic for its destinations is
  automatically rerouted to a healthy upstream within seconds.
- Live observability of the tunnel pool state via the `status` subcommand.
- Configuration changes (adding/removing upstreams) applied without restarting
  and without dropping active connections.

### 1.3 Non-Goals

- Not a generic SOCKS5/HTTP proxy with authentication, ACLs, or per-user
  isolation. It listens on loopback only and assumes a single trusted user.
- Not a replacement for VPN tooling (no full-tunnel routing, no UDP, no DNS over
  the tunnel beyond what destination-host resolution requires).
- Not a load balancer for distributed services. The balancing logic optimizes
  for session stability, not throughput.
- Not a learning platform for new contributors. It is a personal project;
  contributions are welcome but not solicited.

### 1.4 Target Platforms

- **Primary:** macOS (Apple Silicon and Intel).
- **Secondary, post-v1.0:** Linux (x86_64 and aarch64).
- **Possible, no commitment:** Windows.

Architecture choices throughout this specification favor cross-platform
abstractions where they cost nothing, while platform-specific implementations
are deferred until concretely needed.

---

## 2. Architecture

### 2.1 Process Model

Two operating modes for a single binary:

- **Daemon mode** (`proxynaut start`): long-running process that owns the SSH
  pool, listeners, health checker, and control socket.
- **Client mode** (`proxynaut status`, `reload`, `stop`, `logs`): short-lived
  process that connects to the daemon's control socket over IPC and exits.

Exactly one daemon process per user account is expected to be running at a time.
Multiple daemons are not prevented by the binary itself but are not supported.

### 2.2 Component Overview

```
                ┌─────────────────────────────────────────────────────┐
                │                    proxynaut daemon                 │
                │                                                     │
   client       │   ┌──────────┐                                      │
  ─────────────▶┤───▶ SOCKS5   ├──┐                                   │
   :1080        │   │ listener │  │                                   │
                │   └──────────┘  │   ┌──────────┐   ┌────────────┐   │
                │                 ├──▶│ Balancer ├──▶│ SSH pool   │   │
                │   ┌──────────┐  │   │ (cons.   │   │ + channel  │   │
  ─────────────▶┤───▶  HTTP    ├──┘   │  hashing)│   │ multiplex  │──▶│──▶ VPS pool
   :8118        │   │  CONNECT │      └────▲─────┘   └─────▲──────┘   │
                │   └──────────┘           │               │          │
                │                          │               │          │
                │   ┌──────────────────────┴───────────────┘          │
                │   │  Health checker  ◀── periodic ───────┐          │
                │   └──────────────────────────────────────┘          │
                │                                                     │
                │   ┌──────────┐                                      │
   IPC          │   │ Control  │                                      │
  ─────────────▶┤───▶  socket  │── status / reload / stop ─▶ runtime  │
                │   └──────────┘                                      │
                └─────────────────────────────────────────────────────┘
```

### 2.3 Component Responsibilities

| Component         | Responsibility                                                                                                                |
| ----------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Config loader     | Parse and validate `config.toml`; expose typed configuration to other components.                                             |
| SSH pool          | Maintain long-lived SSH sessions to each enabled upstream; open `direct-tcpip` channels on demand; auto-reconnect on failure. |
| Health checker    | Periodically probe each upstream via its SSH session; update `Healthy`/`Dead` state; notify balancer of changes.              |
| Balancer          | Maintain a consistent-hash ring of healthy upstreams; given a destination host, return the assigned upstream.                 |
| SOCKS5 listener   | Accept SOCKS5 client connections; parse handshake and CONNECT request; delegate to balancer + pool.                           |
| HTTP listener     | Accept HTTP CONNECT requests; parse target; delegate to balancer + pool.                                                      |
| Control socket    | Accept IPC connections from `proxynaut` client subcommands; serve status snapshots; trigger reload.                           |
| Daemon supervisor | Wire components together; handle signals; coordinate graceful shutdown.                                                       |

### 2.4 Concurrency Model

A single Tokio multi-threaded runtime drives the entire daemon. Components
communicate via:

- **Channels** (`tokio::sync::mpsc`, `broadcast`) for events (health state
  changes, config reload, shutdown).
- **Shared state** (`Arc<RwLock<…>>` or `Arc<ArcSwap<…>>`) for the balancer's
  hash ring and the pool status snapshot read by `status`.

Every accepted client connection is handled in its own spawned task. The task
lives for the duration of the proxied TCP session.

---

## 3. Components

### 3.1 Configuration

**Location:** `~/Library/Application Support/proxynaut/config.toml` on macOS,
resolved via the `directories` crate. The path is included in `proxynaut --help`
output.

**Format:** TOML.

**Schema:** see Section 5.

**Behavior on missing file:** `proxynaut start` exits with an error message
suggesting `proxynaut init` (which writes a template config and exits).

**Validation rules:**

- At least one enabled upstream must be defined.
- Every `jump_host` referenced by an upstream must be defined in the
  `[jump_host.*]` section.
- `host:port` addresses for listeners must be valid and on `127.0.0.1` or `::1`
  (loopback enforced for security).
- Weight must be a positive integer.
- Key files referenced in configuration must exist and be readable at the time
  of `proxynaut check` or `start`.
- File permissions on `config.toml` must be `0600` or stricter on macOS and
  Linux; warn (do not fail) if looser.

**Reload semantics:** `proxynaut reload` causes the daemon to:

1. Re-read and re-validate the config.
2. Identify added, removed, and modified upstreams by name.
3. For removed/disabled upstreams: drain (stop opening new channels) and close
   the SSH session after current channels finish, or after a 30-second grace
   period.
4. For added upstreams: start a new SSH session and let the health checker
   evaluate it.
5. For modified upstreams: if connection-relevant fields changed (host, port,
   user, key, jump), tear down and re-establish; if only weight or `enabled`
   changed, update in-place.

### 3.2 SSH Connection Pool

**Lifecycle of an upstream:**

```
        ┌────────────────────────────────────────────────────┐
        ▼                                                    │
   ┌──────────┐    auth        ┌─────────┐  hc fail ×3  ┌────┴───┐
   │Connecting├───────────────▶│ Healthy ├─────────────▶│  Dead  │
   └────┬─────┘                └────┬────┘              └────┬───┘
        │                           │                        │
        │ fail                      │ session drop           │ reconnect
        │                           │                        │ (backoff)
        └───────────────────────────┴────────────────────────┘
```

**Connection establishment:**

- Open TCP to upstream `host:port`.
- If `jump_host` is configured, open TCP to the jump host instead, perform SSH
  handshake, then open a `direct-tcpip` channel to the final upstream through
  it, and wrap the channel as an `AsyncRead + AsyncWrite` stream on which to run
  a second SSH client session.
- Authenticate with the configured private key (file-based in v0.1; `ssh-agent`
  support added in v0.2).
- Optionally verify the host key against `~/.ssh/known_hosts` (controlled by
  per-upstream or global `strict_host_key_checking` setting; default `true`).
- Mark upstream as `Connecting → Healthy` after first successful health check.

**Channel management:**

- One long-lived SSH session per upstream.
- One `direct-tcpip` channel per client connection.
- No channel reuse or pooling. Each client-server TCP session maps 1:1 to one
  SSH channel.
- Channel limit: rely on `russh` defaults (~256 simultaneous), well above
  expected load.

**Keepalive:**

- Send SSH keepalive every 30 seconds.
- Mark session dead after 3 missed responses.
- Hardcoded; not exposed in config in v0.1.

**Reconnect strategy:**

- On session drop, mark `Dead`, schedule reconnect with exponential backoff
  starting at 1 second, doubling up to 60 seconds maximum.
- Apply ±20% random jitter to each delay.
- Retry indefinitely; no maximum attempt count.
- On successful reconnect, return to `Connecting` state until health check
  passes.

### 3.3 Health Checker

**Mechanism (v0.4):** open a `direct-tcpip` channel through the session to the
configured target (default `1.1.1.1:443`), with a timeout. Success = channel
opens within the timeout. Close the channel immediately after.

**State machine:**

- `Connecting`: session is being established; balancer treats as unavailable.
- `Healthy`: last `recovery_threshold` (default 2) consecutive checks succeeded;
  balancer includes in ring.
- `Dead`: last `failure_threshold` (default 3) consecutive checks failed;
  balancer excludes from ring; session may still be alive (we let it run and
  continue checking).

**Frequency:** `interval_sec` per upstream, applied independently. Default: 30
seconds. Health checks must not overlap for the same upstream — if a check is
still running when the next is due, skip and log a warning.

**Transitions trigger balancer ring rebuild.**

### 3.4 Balancer

**Algorithm:** consistent hashing via the `hashring` crate.

- Ring built from currently `Healthy` upstreams.
- Per-upstream virtual nodes count proportional to weight (e.g., weight 1 = 150
  virtual nodes, weight 2 = 300, weight 3 = 450).
- Key fed into the ring: the destination host (DNS name or IP string, lowercase,
  without port). Rationale: same host across multiple connections must hit the
  same upstream; port is irrelevant for affinity.
- Ring is wrapped in `ArcSwap` for lock-free reads from many tasks; only rebuilt
  on health state change or config reload.

**Selection contract:** given a destination host, return the assigned upstream's
identifier. If no healthy upstreams exist, return an error; the listener
responds to the client with a SOCKS5 / HTTP failure code.

**Future extensions (post-v1.0):** least-connections fallback when multiple
upstreams have identical hash positions; per-destination overrides; manual
pinning via control socket.

### 3.5 SOCKS5 Listener

**Subset implemented (v0.1):**

- Authentication method: `0x00 NO AUTHENTICATION REQUIRED` only.
- Command: `0x01 CONNECT` only. `BIND` and `UDP ASSOCIATE` rejected with reply
  code `0x07 (Command not supported)`.
- Address types: `0x01 IPv4`, `0x03 DOMAIN`, `0x04 IPv6`.
- Reply codes: per RFC 1928 — `0x00 success`, `0x01 general failure`,
  `0x03 network unreachable`, `0x05 connection refused`,
  `0x07 command not supported`, etc.

**Flow:**

1. Accept TCP on `local.socks5_addr`.
2. Read greeting, reply with `0x05 0x00`.
3. Read request, parse target host and port.
4. Ask balancer for upstream for `target_host`.
5. Open `direct-tcpip` channel through that upstream's SSH session to
   `target_host:target_port`.
6. Send SOCKS5 reply (success with bound address `0.0.0.0:0` is acceptable per
   RFC; we use this rather than reporting the real remote address).
7. `tokio::io::copy_bidirectional` between client TCP and SSH channel until
   either side closes.

### 3.6 HTTP Listener

**Subset implemented (v0.5):**

- HTTP/1.1 CONNECT method only.
- No support for plain HTTP proxying (GET/POST through the proxy without
  CONNECT). Modern clients use HTTPS exclusively, and Claude Code in particular
  uses HTTPS.
- No proxy authentication.

**Flow:**

1. Accept TCP on `local.http_addr`.
2. Read request line and headers; require `CONNECT host:port HTTP/1.1`.
3. Ask balancer for upstream for `host`.
4. Open `direct-tcpip` channel to `host:port`.
5. Reply `HTTP/1.1 200 Connection established\r\n\r\n` to the client.
6. `copy_bidirectional` until close.

Non-CONNECT requests respond with `HTTP/1.1 405 Method Not Allowed`.

### 3.7 Control Interface

**Transport:** local socket via the `interprocess` crate.

- macOS/Linux: Unix domain socket at
  `~/Library/Application Support/proxynaut/control.sock` (macOS) or
  `$XDG_RUNTIME_DIR/proxynaut/control.sock` (Linux).
- Windows (future): named pipe `\\.\pipe\proxynaut`.

**Protocol:** line-delimited JSON. One request per line, one response per line,
connection closed after response.

Request shape:

```json
{"cmd": "status"}
{"cmd": "reload"}
{"cmd": "stop"}
```

Response shape:

```json
{"ok": true, "data": { ... }}
{"ok": false, "error": "message"}
```

`status` response example:

```json
{
  "ok": true,
  "data": {
    "uptime_sec": 3621,
    "config_path": "/Users/sergey/.../config.toml",
    "upstreams": [
      {
        "name": "do-nyc",
        "state": "Healthy",
        "active_channels": 3,
        "total_bytes_sent": 142857,
        "total_bytes_received": 9381723,
        "last_health_check_sec_ago": 12,
        "consecutive_successes": 47,
        "consecutive_failures": 0
      },
      {
        "name": "is-fra",
        "state": "Dead",
        "active_channels": 0,
        "total_bytes_sent": 0,
        "total_bytes_received": 0,
        "last_health_check_sec_ago": 8,
        "consecutive_successes": 0,
        "consecutive_failures": 5
      }
    ]
  }
}
```

**Security:** the control socket is created with `0600` permissions so only the
daemon's user can connect. No authentication beyond filesystem permissions.

---

## 4. Command-Line Interface

### 4.1 Subcommands

| Command                                | Purpose                                                                                                     |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `proxynaut init`                       | Write a template `config.toml` to the default location if none exists. Print the path.                      |
| `proxynaut check`                      | Parse and validate the config; report errors and exit. Does not connect to anything.                        |
| `proxynaut start [--foreground]`       | Start the daemon. With `--foreground`, attach to terminal and log to stderr; without it, daemonize (v0.6+). |
| `proxynaut stop`                       | Send shutdown command via control socket; wait for daemon to exit.                                          |
| `proxynaut status [--json]`            | Print pool status. Default: human-readable table. With `--json`: raw JSON from control socket.              |
| `proxynaut reload`                     | Trigger config reload in the running daemon.                                                                |
| `proxynaut logs [--tail N] [--follow]` | Show daemon log file contents (v0.6+; foreground mode logs to stderr directly).                             |

### 4.2 Global Flags

| Flag               | Purpose                                                                      |
| ------------------ | ---------------------------------------------------------------------------- |
| `--config PATH`    | Override default config path. Applies to `start`, `check`, `init`, `reload`. |
| `-v` / `-vv`       | Increase log verbosity to `debug` / `trace`. Overrides env var.              |
| `--help` / `-h`    | Standard help.                                                               |
| `--version` / `-V` | Print version and exit.                                                      |

### 4.3 Exit Codes

| Code | Meaning                                                       |
| ---- | ------------------------------------------------------------- |
| 0    | Success.                                                      |
| 1    | Generic failure.                                              |
| 2    | Configuration error (invalid file, missing required fields).  |
| 3    | Daemon not running (for client commands that need it).        |
| 4    | Daemon already running (for `start`).                         |
| 5    | IPC failure (control socket unreachable, malformed response). |

---

## 5. Configuration Schema

### 5.1 Full Example

```toml
[local]
socks5_addr = "127.0.0.1:1080"
http_addr   = "127.0.0.1:8118"

[upstream.do-nyc]
host       = "1.2.3.4"
port       = 22
user       = "tunnel"
key_path   = "~/.ssh/id_ed25519"
jump_host  = "stackra"          # optional, references [jump_host.stackra]
weight     = 2                  # optional, default 1
enabled    = true               # optional, default true
strict_host_key_checking = true # optional, default true

[upstream.is-fra]
host     = "5.6.7.8"
port     = 22
user     = "tunnel"
key_path = "~/.ssh/id_ed25519"

[jump_host.stackra]
host     = "stackra.example.com"
port     = 22
user     = "sergey"
key_path = "~/.ssh/id_ed25519"
strict_host_key_checking = true

[healthcheck]
interval_sec       = 30         # optional, default 30
timeout_sec        = 5          # optional, default 5
target             = "1.1.1.1:443" # optional, default "1.1.1.1:443"
failure_threshold  = 3          # optional, default 3
recovery_threshold = 2          # optional, default 2

[log]
level = "info"                  # optional, default "info"
```

### 5.2 Defaults

All keys marked optional take effect even if absent. The `init` template
includes all keys with their defaults shown commented out.

### 5.3 Restrictions

- Section names under `[upstream.*]` and `[jump_host.*]` must match
  `^[a-z0-9][a-z0-9-]*$`. Used as identifiers in logs and `status` output.
- `weight` range: 1–100. Out-of-range values are clamped with a warning.
- Listener addresses must resolve to a loopback interface.

---

## 6. Logging

### 6.1 Levels

| Level   | Used for                                                                                                     |
| ------- | ------------------------------------------------------------------------------------------------------------ |
| `error` | Failures requiring user attention: config errors, auth failures, all upstreams dead.                         |
| `warn`  | Recoverable issues: single health check failure, slow check, looser-than-recommended file perms.             |
| `info`  | Lifecycle events: daemon start/stop, upstream state transitions, config reload outcome, SSH session up/down. |
| `debug` | Per-connection events: client accepted, target host, selected upstream, channel opened/closed.               |
| `trace` | Protocol-level details: SOCKS5/HTTP handshake bytes, SSH channel data summaries.                             |

Default level: `info`.

### 6.2 Outputs

- `proxynaut start --foreground`: stderr, with ANSI colors when stderr is a TTY.
- `proxynaut start` (daemonized, v0.6+): file
  `~/Library/Logs/proxynaut/proxynaut.log`, rotated daily, retaining 7 days, via
  `tracing-appender::rolling`.

### 6.3 Format

Text only. Single line per event:

```
2026-04-17T07:42:01.234Z  INFO  upstream{name="do-nyc"} state changed: Connecting -> Healthy
2026-04-17T07:42:03.118Z  WARN  upstream{name="is-fra"} health check failed: timeout after 5s (consecutive: 1)
```

JSON-formatted logs are not implemented; can be added later if external log
aggregation is wanted.

### 6.4 Configuration Sources

Precedence (later wins):

1. Compiled-in default (`info`).
2. `[log] level = "..."` in config.
3. `PROXYNAUT_LOG` environment variable (accepts the full
   `tracing_subscriber::EnvFilter` syntax for targeted module overrides).
4. `-v` / `-vv` CLI flag.

---

## 7. Security

### 7.1 Threat Model

- Single-user machine. No multi-tenant isolation.
- All listeners on loopback only. Refuse to bind to non-loopback addresses in
  the validator.
- IPC socket created with `0600` permissions.
- Config file expected `0600`, warned if looser.

### 7.2 SSH Key Handling

- Keys read from filesystem only (v0.1); `ssh-agent` integration is v0.2.
- Passphrase-protected keys prompt interactively via `rpassword` at daemon
  start. If multiple keys need passphrases, prompt for each in sequence.
- Passphrases never written to disk or logged. Keys held in memory for the
  lifetime of the daemon.
- No support for password authentication. Ever.

### 7.3 Host Key Verification

- `strict_host_key_checking = true` by default.
- Read from `~/.ssh/known_hosts` (or `~/.ssh/known_hosts2` if present).
- On mismatch, refuse to connect and log an error.
- New unknown hosts: in v0.1, refuse to connect and instruct the user to add the
  key manually (most explicit option). TOFU acceptance via a config flag may be
  added later.

### 7.4 Data in Transit

All traffic between proxynaut and upstreams travels inside SSH channels, which
are encrypted by definition. The client-side traffic (proxynaut <- local apps
over loopback) is unencrypted at the TCP level but never leaves the machine.

---

## 8. Cross-Platform Considerations

### 8.1 Abstractions Adopted from the Start

- **Paths:** `directories` crate for config, data, log directories. Never
  hardcode `~/Library/...` or `/etc/...`.
- **IPC:** `interprocess` crate. Use the `tokio` integration. Same code path on
  Unix and Windows.
- **Signals:** `tokio::signal::ctrl_c()` works everywhere; SIGHUP for reload is
  Unix-only and gated behind `#[cfg(unix)]`. Reload via control socket works on
  all platforms.
- **Terminal colors:** `tracing-subscriber` with ANSI; respects `NO_COLOR` env
  var.

### 8.2 macOS-Specific in v0.1

- LaunchAgent template provided as a separate doc (not auto-installed by the
  binary).
- Default log path uses `~/Library/Logs/...`.

### 8.3 Deferred Cross-Platform Work

- Linux: systemd user-unit template; XDG-conformant log path.
- Windows: named pipe IPC (via `interprocess`, should work out of the box);
  ACL-based config permission checks; OpenSSH agent integration.

These are listed in the Roadmap but not part of v1.0 commitment.

---

## 9. Technology Stack

### 9.1 Required Crates

| Crate                            | Purpose                 | Notes                                    |
| -------------------------------- | ----------------------- | ---------------------------------------- |
| `tokio`                          | Async runtime           | Features `full` for v0.1; trim later.    |
| `russh`                          | SSH client              | Pure-Rust, no system OpenSSH dependency. |
| `russh-keys`                     | SSH key parsing         | Used with `russh`.                       |
| `hashring`                       | Consistent hashing      | Simple API, weight support.              |
| `serde` + `serde_derive`         | Serialization           | For config and IPC protocol.             |
| `toml`                           | Config parsing          | Standard for Rust.                       |
| `serde_json`                     | IPC protocol            | Line-delimited JSON.                     |
| `clap` (derive)                  | CLI parsing             | v4.x.                                    |
| `tracing` + `tracing-subscriber` | Logging                 | With `EnvFilter`.                        |
| `tracing-appender`               | Log rotation            | For daemon mode (v0.6+).                 |
| `directories`                    | XDG/macOS paths         | Cross-platform from day one.             |
| `interprocess` (tokio)           | IPC                     | Cross-platform local socket.             |
| `thiserror`                      | Error types in lib      | For `proxynaut-core`.                    |
| `anyhow`                         | Error handling in bin   | For `proxynaut-cli`.                     |
| `rpassword`                      | Secure passphrase input | Cross-platform terminal.                 |

### 9.2 Disallowed Approaches

- No `#[cfg(...)]`-heavy code in core modules. Platform branches live in thin
  adapter modules only.
- No `unsafe` outside well-justified cases (none currently expected).
- No `unwrap()` in non-test code. Use proper error propagation via `thiserror`
  (in lib) and `anyhow` (in bin).
- No blocking I/O in async contexts. All file reads at startup are acceptable to
  be synchronous; runtime operations must be async.

---

## 10. Repository Structure

### 10.1 Workspace Layout

```
proxynaut/
├── Cargo.toml                 # workspace manifest
├── Cargo.lock
├── rust-toolchain.toml        # stable channel
├── .gitignore
├── .github/
│   └── workflows/
│       └── ci.yml             # cargo fmt --check, clippy, test
├── README.md                  # user-facing intro
├── LICENSE
├── docs/
│   ├── SPEC.md                # this document
│   ├── architecture.md        # diagrams, deeper explanations
│   ├── config-reference.md    # exhaustive config docs
│   └── launchd.md             # macOS auto-start instructions
├── examples/
│   └── config.toml            # commented example
├── crates/
│   ├── proxynaut-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs
│   │       ├── ssh/
│   │       │   ├── mod.rs
│   │       │   ├── session.rs
│   │       │   ├── pool.rs
│   │       │   └── reconnect.rs
│   │       ├── health.rs
│   │       ├── balancer.rs
│   │       ├── proxy/
│   │       │   ├── mod.rs
│   │       │   ├── socks5.rs
│   │       │   └── http.rs
│   │       ├── control/
│   │       │   ├── mod.rs
│   │       │   ├── protocol.rs
│   │       │   └── server.rs
│   │       ├── daemon.rs
│   │       └── error.rs
│   └── proxynaut-cli/
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── commands/
│           │   ├── mod.rs
│           │   ├── start.rs
│           │   ├── stop.rs
│           │   ├── status.rs
│           │   ├── reload.rs
│           │   ├── check.rs
│           │   ├── init.rs
│           │   └── logs.rs
│           └── client.rs       # IPC client
└── tests/
    └── integration/
        └── (added from v0.2+)
```

### 10.2 Workspace Manifest

`Cargo.toml` at root declares both crates as workspace members and centralizes
common dependency versions via `[workspace.dependencies]` for consistency.

### 10.3 Versioning

- Both crates share a single version, bumped together.
- Pre-1.0 (current): minor version bumps may break API/config; patch versions
  are bug-fix only.
- Post-1.0: strict SemVer.

---

## 11. Development Roadmap

Each version is one milestone; one or more features at a time, tests alongside
the code, conventional commits.

### v0.1 — SOCKS5 through a single SSH session

- Workspace skeleton, both crates, CI green.
- Config loading and validation (single upstream only acceptable).
- File-based key auth with passphrase prompt.
- SSH session establishment via `russh` to one upstream (no jump host).
- SOCKS5 listener with CONNECT support.
- Foreground-only operation; `--foreground` is implicit.
- `proxynaut init`, `check`, `start --foreground`. No daemonization yet.
- Manual smoke test: configure for one VPS, run, curl through 127.0.0.1:1080.

### v0.2 — Jump host + ssh-agent

- `jump_host` support in config; nested SSH session via `direct-tcpip` channel
  through the jump host's SSH connection.
- `ssh-agent` integration as an alternative to file-based keys.
- Integration test infrastructure: dockerized `openssh-server` in
  `tests/integration/`.

### v0.3 — Pool and round-robin balancing

- Multiple upstreams in config.
- Naive round-robin balancing (no consistent hashing yet).
- No health checks; assume all upstreams healthy.

### v0.4 — Health checks and consistent hashing

- Health checker with state machine and thresholds.
- Consistent-hash balancer (sticky-by-destination).
- Weights respected via virtual-node count.
- State changes logged at `info`.

### v0.5 — HTTP proxy

- HTTP CONNECT listener on the configured port.
- Same balancer and pool as SOCKS5.
- Privoxy can be uninstalled.

### v0.6 — Daemonization and control socket

- Background mode via `fork`/`setsid` or `daemonize` crate.
- Log rotation via `tracing-appender`.
- Control socket with status, reload, stop commands.
- `proxynaut status` (table output), `proxynaut reload`, `proxynaut stop`,
  `proxynaut logs --tail`.

### v0.7 — Polish

- `proxynaut status --json` for scripting.
- Per-upstream metrics: byte counters, latency histogram.
- Graceful drain on reload (don't kill active channels for removed upstreams;
  wait for them to finish).

### v1.0 — Documentation and release

- README with quickstart and screenshots/asciinema.
- LaunchAgent template and installation instructions.
- GitHub Releases with macOS binaries (aarch64 + x86_64).
- Homebrew tap (optional).

### Post-v1.0 (no commitment)

- Linux support: systemd user-unit, package builds.
- Windows support: named pipe IPC (mostly free via `interprocess`), OpenSSH
  agent on Windows, ACL-based perm checks.
- Per-destination overrides in config (force-route specific hosts).
- TUI status view (replacing or supplementing `status` table).
- Metrics endpoint (Prometheus format).

---

## 12. Out of Scope

Explicitly not in this project, now or later:

- DNS resolution proxying. Destination hosts are resolved by the upstream server
  (SOCKS5 domain mode), so local DNS is irrelevant.
- UDP proxying.
- Transparent proxying (`pf` redirect, `iptables -j REDIRECT`, etc.).
- Protocol-level inspection or filtering of proxied traffic.
- Captive-portal or browser-extension integrations.
- Per-application routing rules.
- Encrypted config storage. Filesystem permissions are deemed sufficient.

---

## 13. Glossary

| Term                  | Meaning                                                                                                                                                  |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Upstream              | A remote VPS that proxynaut tunnels traffic through, identified by name in config.                                                                       |
| Jump host             | An intermediate SSH server traversed before reaching an upstream; modeled as a separate SSH connection nested inside which the upstream connection runs. |
| Session               | A long-lived SSH connection to a single upstream, lasting until network failure or shutdown.                                                             |
| Channel               | A single `direct-tcpip` SSH channel inside a session, mapped 1:1 to a client connection.                                                                 |
| Destination host      | The remote target the client wants to reach (e.g. `api.anthropic.com`); the key used for balancer affinity.                                              |
| Sticky-by-destination | Routing policy where all connections to the same destination host go through the same upstream (when healthy).                                           |
| Health check          | A periodic probe that opens a test channel through an upstream's session to confirm it can carry traffic.                                                |
| Healthy/Dead          | The two terminal states of an upstream; `Connecting` is the transitional state during startup or reconnect.                                              |

---

## Appendix A: Decisions Log

Rationale for non-obvious choices, for future me.

- **Consistent hashing over round-robin:** session-affinity matters more than
  uniform distribution for the target workload (OAuth APIs, web services with
  cookies). Round-robin was rejected on day one.
- **`interprocess` crate over hand-rolled UDS:** cross-platform IPC was cheap to
  adopt upfront; switching later would be intrusive.
- **`russh` over wrapping system `ssh` binary:** the wrapper approach (`ssh -W`,
  `ssh -D`) is brittle for programmatic channel management and health checking;
  pure-Rust SSH gives full control.
- **Workspace from day one despite small initial size:** core/cli separation
  forces clear API boundaries from the start, simplifies testing, and avoids a
  disruptive refactor later.
- **No GUI:** CLI-first matches the daily workflow (terminal-driven), and a GUI
  can be added later as a thin client over the existing control socket without
  changes to core.
