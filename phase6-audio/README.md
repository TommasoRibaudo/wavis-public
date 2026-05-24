# Phase 6 Audio Harness

This folder is for local-only real-audio validation of the experimental
DeepFilterNet noise-suppression backend.

1. Record short 10-20 second WAV samples.
2. Export or convert them to 48 kHz WAV. The harness does not resample.
3. Put the files in `phase6-audio/input/`.
4. Run:

```powershell
cargo run -p wavis-client-shared --features real-backends,deepfilternet-backend --example noise_suppression_file -- --attempt compensated-baseline-01 phase6-audio/input/<sample>.wav
```

The generated comparison files are written to
`phase6-audio/output/<attempt-name>/`. The harness fails if that attempt folder
already exists unless `--overwrite` is passed. Existing flat files directly in
`phase6-audio/output/` are historical listening artifacts and should be left in
place.

- `<sample>_none.wav`
- `<sample>_rnnoise.wav`
- `<sample>_deepfilternet_full.wav`
- `<sample>_deepfilternet_atten12.wav`
- `<sample>_deepfilternet_atten18.wav`
- `<sample>_deepfilternet_quiet_bypass.wav`
- `<sample>_deepfilternet_blend25.wav`
- `<sample>_deepfilternet_blend50.wav`
- `<sample>_deepfilternet_blend75.wav`
- `<sample>_deepfilternet_onset_safe_blend25.wav`

Each attempt folder also contains `attempt_manifest.json` with the command,
timestamp, git summary, inputs, backend timing, finite/output length status,
latency samples, and frame-boundary correlation diagnostics.

Record listening notes in `DEEPFILTERNET_NOISE_SUPPRESSION_PHASED_TASKS.md`
under Phase 6. Do not commit real recordings or generated audio.
