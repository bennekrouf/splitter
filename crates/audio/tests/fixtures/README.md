`video.mp4`: 12 s of the tests' `signal()` (as written by `write_wav`) as 64 kbps AAC, with a
tiny H.264 picture. PNS is off: its noise bands are random, so a seek couldn't match a straight
decode sample for sample.

```bash
ffmpeg -f lavfi -i "color=c=gray:size=32x18:rate=5:duration=12" -i transcode-src.wav \
    -c:v libx264 -pix_fmt yuv420p -c:a aac -aac_pns 0 -b:a 64k -movflags +faststart video.mp4
```
