# art3m1s-log

Reusable logging boundary for Art3m1s runtimes.

This crate is intentionally independent from:

- `art3m1s-core`;
- GPU/backend crates;
- interpreters and game engines;
- platform FFI.

It exposes a cloneable `Logger` with a replaceable `Sink` and optional
`Filter`, plus a one-time bridge from the standard `log` facade.

The host-specific FFI callback remains outside this crate.
