# Pinned Upstream

The native adapter must use these revisions:

| Component | Repository | Revision |
| --- | --- | --- |
| KRKR runtime | <https://github.com/krkrsdl3/krkrsdl3.git> | `6c7570e2088223dd7741938512c73fc6e3dee887` |
| KRKR build system | <https://github.com/krkrsdl3/krkrsdl3_build.git> | `66fb7d9533478d33317208cd8ec8696ab9340d6f` |

At the time this pin was recorded, the build repository's `cpp` gitlink still
referenced `5a8bd422f82d3758045f403520a64b772a59f40c`. Do not initialize or
build the submodule at that revision. The native CMake project must consume the
source checkout passed through `KRKRSDL3_SOURCE_DIR`.

The protocol was checked against the newer runtime revision, including the
`DrawMesh` color-modulation signature change. Capture remains anchored at the
window texture present path, so that signature change does not alter the RGBA
frame ABI.
