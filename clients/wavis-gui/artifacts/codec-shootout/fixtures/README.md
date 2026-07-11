# Codec Shootout — Content Corpus Fixtures

These fixtures provide deterministic, replayable content sources for the W4
codec-policy shootout. Both sources must be present before `run-matrix.mjs`
will start a non-dry-run.

## Required fixtures

### `detail/` — Detail-heavy source

A scripted terminal + IDE scroll through a fixed source file.

**Format:** recorded desktop capture at 1440p30 in a lossless format (e.g. FFV1 in MKV) suitable for replay via a virtual-display or screen-share fixture player.

**Content:** slow scroll through `clients/wavis-gui/src/features/voice/livekit-media.ts` in VS Code with syntax highlighting, at ~60 lines/second, looping for at least 120 seconds. No cursor movement outside the file.

**Why:** exercises Detail_Profile with static, high-contrast text. Representative of "code review" sharing.

**To create:**

```
# Record using OBS / FFmpeg on a 2560×1440 display:
# 1. Open livekit-media.ts in VS Code at 12pt font with a dark theme
# 2. Run a scripted scroll (xdotool, AutoHotkey, or Karabiner)
# 3. Capture at 1440p30 for 180 seconds
# 4. Encode: ffmpeg -i raw.mkv -c:v ffv1 artifacts/codec-shootout/fixtures/detail/scroll.mkv
```

### `motion/` — Motion-heavy source

A 1080p60 demo clip with pan + zoom segments.

**Format:** lossless 1920×1080 60 fps (e.g. FFV1 in MKV) for deterministic replay.

**Content:** at least 120 seconds of continuous motion — use a screensaver, game replay, or 1080p60 test clip with pan+zoom (e.g. Big Buck Bunny 1080p60). No static frames lasting more than 1 second.

**Why:** exercises Motion_Profile with high frame-over-frame change. Representative of "demo video" sharing.

**To create:**

```
# Use any 1080p60 source clip:
ffmpeg -i source_1080p60.mp4 \
  -c:v ffv1 -level 3 \
  -vf "scale=1920:1080" \
  -t 180 \
  artifacts/codec-shootout/fixtures/motion/clip.mkv
```

## Notes

- Fixtures are committed to the repository so that runs are reproducible across machines.
- Binary fixture files must NOT be tracked by Git LFS unless the project already uses it; use a shared network path or object store instead and add the paths to `.gitignore`.
- The runner looks for the directory `fixtures/detail/` and `fixtures/motion/` to exist; the specific file names within those directories are passed to the publisher process via `WAVIS_SHOOTOUT_FIXTURE_DIR` + `WAVIS_SHOOTOUT_CONTENT_SOURCE`.
