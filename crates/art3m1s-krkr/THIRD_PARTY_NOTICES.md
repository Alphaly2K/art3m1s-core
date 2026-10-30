# Third-party notices

The native host compiles external KRKRSDL3 sources and links SDL3, SDL3_ttf,
FreeType, PNG, JPEG, WebP, Vorbis, Ogg, Opus, OpusFile, Oniguruma, plutovg,
zlib and their enabled dependencies. FFmpeg is included on targets that enable
its wave decoder. The application build copies the vcpkg package copyright
files alongside the host's license notices.

KRKRSDL3 is distributed under the terms in its upstream `LICENSE`:

<https://github.com/krkrsdl3/krkrsdl3/blob/main/LICENSE>

The upstream license includes a supplementary condition for binary ports of
commercial games that were not originally developed with KRKRSDL3. If the
KRKRSDL3 code itself is modified, complete corresponding modifications must
be made publicly available under the same terms when the binary is distributed.

Distribution builds must retain upstream copyright and license texts and
publish corresponding source and build materials required by enabled copyleft
dependencies. The FFmpeg build must use a license configuration compatible
with the application's distribution terms.
