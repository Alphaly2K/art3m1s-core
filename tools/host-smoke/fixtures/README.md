`blue-theora.ogv` contains 24 synthetic blue frames at 32 × 32, with no audio.
The media Host probe checks the decoded pixels, timestamps, end of stream and
absent audio track. An equivalent fixture can be generated with a build of
FFmpeg that supports the `libtheora` encoder.

```sh
ffmpeg -f lavfi -i 'color=blue:s=32x32:r=24:d=1' -c:v libtheora -g 1 -an blue-theora.ogv
```
