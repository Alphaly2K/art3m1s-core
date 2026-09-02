# Eluna compatibility fork

This crate is derived from [xmoezzz/eluna](https://github.com/xmoezzz/eluna)
at commit `12e4d2fa03b64714a83a0363eaadf26a125d9fe6`.

Art3m1s keeps the source in-tree so experimental E-Mote support is reproducible
across desktop and mobile builds. Local changes are intentionally limited to
runtime performance and integration fixes:

- immutable motion-priority maps are shared between traversal contexts;
- read-only PSB objects are borrowed instead of recursively cloned;
- deferred nested-motion records borrow their source layer;
- motion-level parameter bindings drive unbound child layers, including automatic
  eyelid animations; a null layer binding inherits its motion's clock;
- textured shape owners retain their atlas-icon domain for inherited deformation;
- empty drawable frames retain decoded channels but do not submit stale sprites;
- identity patches are omitted from emitted sprite geometry (controller state is
  retained). The host reuses lattice samples and reduces tessellation within a
  0.25-stage-pixel error bound instead of drawing every inherited patch at full
  subdivision.

The host's optional `model_blink_reaches_eyelids_and_renders_distinct_frames`
test exercises open/closed/reopened frames through the production GL renderer.
Set `EMOTE_TEST_MODEL` to a local NekoMiko PSB and run the test with
`--release --features experimental-eluna -- --ignored --test-threads=1` from the
core repository. `EMOTE_TEST_OUTPUT` optionally selects an existing directory
for the three PNG captures; game assets are not bundled with the test.

The crate is distributed under the Mozilla Public License 2.0. See
[`LICENSE`](LICENSE).
