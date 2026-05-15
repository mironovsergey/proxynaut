//! Core library for the proxynaut SOCKS5/HTTP proxy daemon.
//!
//! See `docs/SPEC.md` in the workspace root for the full specification.

/// Crate version, exposed for the CLI's `--version` output and `status`
/// response.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
