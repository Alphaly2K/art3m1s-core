# Eluna compatibility fork

This crate is derived from [xmoezzz/eluna](https://github.com/xmoezzz/eluna)
at commit `12e4d2fa03b64714a83a0363eaadf26a125d9fe6`.

Art3m1s keeps the source in-tree so experimental E-Mote support is reproducible
across desktop and mobile builds. Local changes are intentionally limited to
runtime performance and integration fixes:

- immutable motion-priority maps are shared between traversal contexts;
- read-only PSB objects are borrowed instead of recursively cloned;
- deferred nested-motion records borrow their source layer.

The crate is distributed under the Mozilla Public License 2.0. See
[`LICENSE`](LICENSE).
