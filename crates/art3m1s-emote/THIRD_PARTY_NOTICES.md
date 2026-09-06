# Third-party references

`art3m1s-emote` is distributed as part of art3m1s-core under
MPL-2.0. The implementation is independent Rust code.

The following projects were consulted to identify E-Mote PSB fields and to
cross-check observable playback behavior:

- [Eluna](https://github.com/xmoezzz/eluna), vendored at `crates/eluna`,
  declares MPL-2.0 in its Cargo manifest and [license file](../eluna/LICENSE).
  Its evaluator is used to cross-check transforms, mesh deformation,
  visibility, colour and blend behavior. Portions of the transform helpers
  in `src/render.rs` were adapted from its evaluator; retain this attribution
  and the source project's MPL-2.0 notice for those portions.
- [krkrsdl3/plugins/emoteplayer](https://github.com/krkrsdl3/krkrsdl3/tree/main/plugins/emoteplayer),
  Copyright (c) W.Dee and contributors. The reference repository's
  [LICENSE](https://github.com/krkrsdl3/krkrsdl3/blob/main/LICENSE) permits
  source and binary redistribution under its stated notice, disclaimer and
  endorsement conditions, and includes the `krkrsdl3` supplementary clause
  for modified ports of commercial games.

No krkrsdl3 source code was copied into this crate. Its public field coverage
and renderer structure were used as documentation while implementing the Rust
parser and evaluator.
