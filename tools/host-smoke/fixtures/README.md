`blue-theora.ogv` contains 24 synthetic blue frames at 32 × 32, with no audio.
The media Host probe checks the decoded pixels, timestamps, end of stream and
absent audio track. An equivalent fixture can be generated with a build of
FFmpeg that supports the `libtheora` encoder.

```sh
ffmpeg -f lavfi -i 'color=blue:s=32x32:r=24:d=1' -c:v libtheora -g 1 -an blue-theora.ogv
```

`blue-theora-repeat.ogv` keeps two coded frames from the synthetic clip and
replaces the other frame packets with empty Ogg packets at the same timestamps.
The decoder must produce two blue frames and preserve the hold through the
one-second end time.
