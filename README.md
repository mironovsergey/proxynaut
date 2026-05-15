# proxynaut

A local CLI daemon that exposes SOCKS5 and HTTP proxy endpoints and
multiplexes outgoing traffic across a pool of SSH tunnels, with health
checking, weighted balancing, and sticky-by-destination routing via
consistent hashing.

**Status:** pre-alpha. See [`docs/SPEC.md`](docs/SPEC.md) for the full
specification.
