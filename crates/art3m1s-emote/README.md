# art3m1s-emote

E-Mote parsing, playback, and motion evaluation for Art3m1s.

The crate stays independent of the host and GPU APIs. The root
`art3m1s-core` runtime owns Lua userdata callbacks, current/pending layer
instances, texture uploads, scene placement, and compositor integration.

The PSB container behavior was cross-checked against the MIT-licensed
[`number201724/psbfile`](https://github.com/number201724/psbfile) project. This
crate is an independent Rust implementation and does not depend on FreeMote.

## NekoMiko script contract

`system/adv/emote.lua` uses these engine APIs:

- `e:createEmoteLayer { id, files, width, height }`
- `e:getEmoteLayer { id, next }`
- `layer:setScale(scale, origin_x, origin_y)`
- `layer:setCoord(x, y, z, angle)`
- `layer:setVariable(label, value, frames, easing)`
- `layer:playTimeline(label, flags)`
- `layer:fadeInTimeline(label, frames, easing)`
- `layer:pass()`, `layer:step()`, `layer:skip()`

The game creates a transparent E-Mote surface as the `.0` child of an ordinary
foreground layer. Parent layer transforms, transitions, deletion, and save/load
remain normal Artemis layer operations.

NekoMiko uses four encrypted-header PSB v4 models. Their bodies are plain PSB
data. The parser derives the stream key from the canonical v4 header length and
validates the decrypted Adler-32 checksum before reading the object tree.

## Architecture

- `psb`: PSB v2-v4 header, name trie, strings, objects, lists, resource tables
- `atlas`: DXT5 texture descriptors, icon atlas metadata, compressed resource
  access, and lazy RGBA8 decode
- `timeline`: timeline/variable metadata, looping, and per-track keyframe
  sampling
- `motion`: typed character/motion/layer/frame/content/mesh graph
- `render`: recursive base-motion reference resolution into sorted atlas draw
  items, including signed motion coordinates, parent transforms, and mesh
  interpolation
- `player`: script-facing transforms, variables, timeline positions/weights,
  active timeline samples, main/diff timeline state, and command capture

The core integration adds:

- Lua51 and Luau userdata for the engine and E-Mote layer methods
- current/pending instance promotion for Artemis transitions
- lazy DXT5-to-RGBA atlas uploads with bounded command history
- 4x4 deformation grids expanded to dynamic GL triangle lists
- host-owned layer content injection before scene children, inheriting parent
  transform, opacity, clip, shader, visibility, z-order, and deletion

## Known compatibility gaps

- Exact proprietary frame easing semantics beyond linear interpolation
- Exact SDK behavior of `pass`, `step`, and `skip`; calls are accepted and
  retained as playback mode commands, but do not yet alter timeline sampling
- External-texture E-Mote packages and non-DXT5 atlas formats
- Model variants using control-coordinate mesh data (`cc`); NekoMiko uses the
  supported 4x4 blend-point grids (`bp`)
