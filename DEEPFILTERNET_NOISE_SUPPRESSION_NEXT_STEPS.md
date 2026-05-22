# DeepFilterNet Noise Suppression Next Steps

This document captures the current context and the exact next steps for adding improved client-side noise suppression to Wavis without losing the reasoning behind the current state.

## Current State

Wavis currently has a working RNNoise-based suppressor through `nnnoiseless`.

The current production path is:

```text
Mic capture -> capture meter -> DenoiseFilter/RNNoise -> post-denoise meter -> WebRTC APM -> post-APM meter -> Opus encode
```

The backend/server does not process media and does not need a server update for client-side noise suppression work.

`DenoiseFilter` has already been refactored into a backend host:

- `NoiseSuppressorKind::None`
- `NoiseSuppressorKind::Rnnoise`
- `NoiseSuppressorKind::DeepFilterNetExperimental`
- `InfallibleNoiseSuppressorKind::None`
- `InfallibleNoiseSuppressorKind::Rnnoise`
- `DenoiseFilter::new(enabled)` still defaults to RNNoise.
- `DenoiseFilter::try_with_backend(...)` exists for fallible/experimental backends.
- `is_enabled()` means the raw user toggle.
- `is_custom_suppressor_active()` means a real custom suppressor is active and should disable WebRTC APM noise suppression.

DeepFilterNet is not implemented yet. It is scaffolded only. Constructing `DeepFilterNetExperimental` currently returns a clear error instead of silently falling back to RNNoise.

## Goal

Add a better noise suppressor candidate while preserving RNNoise as the stable default.

The first serious candidate is DeepFilterNet, preferably DeepFilterNet2 if a clean Rust integration path is practical. DeepFilterNet3 can be investigated if DFN2 is blocked.

The goal is not just to make it compile. The goal is to prove that it:

- Works in native Rust client code.
- Builds on Windows first, then Linux/macOS.
- Runs fast enough for live 48 kHz voice.
- Does not damage speech quality.
- Can be packaged without GPL/cloud/GPU/browser/backend-media dependencies.
- Can be selected experimentally without disrupting RNNoise users.

## Important Product Constraint

Do not stack denoisers.

Only one of these should be active at a time:

- RNNoise
- DeepFilterNet
- WebRTC APM noise suppression

If RNNoise or DeepFilterNet is active, WebRTC APM NS must be disabled. AEC/AGC can still run through APM, but APM NS should not double-process the signal.

Generic denoise will not reliably remove a competing human voice. Removing another speaker while preserving the target speaker requires target-speaker enhancement, voice enrollment, mic-array processing, or beamforming. DeepFilterNet may improve background noise, hum, fan noise, and some transient noise, but it should not be treated as "remove other voices" technology.

## Phase 1: Re-Verify Integration Options

Before adding dependencies, re-check the current DeepFilterNet Rust ecosystem because this may change.

Confirm:

- Which Rust crate should be used.
- Whether it supports model loading cleanly.
- Whether DFN2 models are supported or only DFN3 is supported.
- Whether the crate builds on Windows.
- Whether the dependency tree remains MIT/Apache-2.0 compatible.
- Whether it requires libtorch, ONNX Runtime, Python artifacts, GPU runtimes, or other heavy native dependencies.
- Whether inference can run CPU-only.
- Whether models can be redistributed with the app.

Decision gate:

- Continue only if there is a realistic CPU-only, cross-platform, non-GPL, native-client path.
- If the path requires GPL, cloud processing, GPU-only inference, browser APIs, or backend media processing, stop and document the blocker.

## Phase 2: Add Dependency Behind Feature Flag

Only after Phase 1 looks viable:

1. Add the selected DeepFilterNet crate to `clients/shared/Cargo.toml` as optional.
2. Wire it to the existing `deepfilternet-backend` feature.
3. Keep `real-backends` separate. Do not make all real backend users build DeepFilterNet by default.
4. Do not change `DenoiseFilter::new(enabled)`. It must remain infallible and RNNoise-backed.

Expected shape:

```toml
[dependencies]
# Example only. Use the actual crate/version after verification.
deep_filter = { version = "...", optional = true }

[features]
deepfilternet-backend = ["dep:deep_filter"]
```

Run immediately:

```powershell
cargo check -p wavis-client-shared --features real-backends,deepfilternet-backend
cargo clippy -p wavis-client-shared --features real-backends,deepfilternet-backend -- -D warnings
```

Decision gate:

- If dependency compilation fails or brings unacceptable native/runtime requirements, stop before touching the hot path.

## Phase 3: Build an Offline Prototype First

Do not wire DeepFilterNet into live calls yet.

Start with the manual harness:

```text
clients/shared/examples/noise_suppression_measure.rs
```

Extend it so it can run:

- RNNoise
- DeepFilterNetExperimental
- possibly passthrough/None for baseline

The harness should report:

- backend name
- startup/model-load time
- average processing time per 20 ms frame
- max processing time per 20 ms frame
- basic output RMS per test case
- whether output contains NaN/Inf

Synthetic test cases should include:

- silence
- white noise
- pink-ish noise
- hum plus noise
- impulses/clicks
- speech-like formant signal
- speech-like signal plus clicks
- two mixed speech-like signals

Run:

```powershell
cargo run -p wavis-client-shared --features real-backends,deepfilternet-backend --example noise_suppression_measure
```

Decision gate:

- Average processing should be comfortably below 20 ms per frame.
- Max processing spikes should be low enough to avoid audio glitches.
- Startup/model-load time should be acceptable for app startup or delayed backend activation.
- Output must remain finite and in a reasonable f32 audio range.

## Phase 4: Implement DeepFilterNetBackend

Once offline timing is viable, replace the placeholder `DeepFilterNetBackend` with a real implementation.

Implementation constraints:

- Keep it behind `deepfilternet-backend`.
- Do not apply the RNNoise VAD/energy gate to DeepFilterNet output.
- Keep all model/runtime state inside `DeepFilterNetBackend`.
- Implement `NoiseSuppressorBackend`.
- `process_frame(&mut self, frame: &mut [f32])` must accept the Wavis standard 960-sample 48 kHz mono frame.
- If DeepFilterNet internally needs a different hop/window size, buffer internally without changing public call sites.
- `reset()` must clear recurrent or buffered state.
- `is_suppressing()` must return true.
- Construction errors must be explicit. Do not fall back to RNNoise unless the caller explicitly selected RNNoise.

The important code is in:

```text
clients/shared/src/denoise_filter.rs
clients/shared/src/webrtc_apm.rs
```

## Phase 5: Add Focused Tests

Add tests that do not require real audio hardware.

Required tests:

- DeepFilterNet constructor returns a useful error when model/runtime files are missing.
- DeepFilterNet backend does not use the RNNoise gate.
- DeepFilterNet `reset()` clears backend state.
- `NoiseSuppressorKind::DeepFilterNetExperimental` active disables APM NS.
- `NoiseSuppressorKind::None` does not disable APM NS.
- `DenoiseFilter::new(enabled)` still selects RNNoise.
- `DenoiseFilter::with_infallible_backend(...)` cannot select DeepFilterNet.
- No NaN/Inf output from DeepFilterNet on deterministic inputs.
- Output length remains exactly 960 samples.

If constructing a real DeepFilterNet backend requires model files, keep heavy model-file tests out of normal unit tests. Use the manual harness or a dedicated feature/test-data path instead.

## Phase 6: Real Audio Quality Validation

After the offline harness passes, test with real recordings before live UI exposure.

Use recordings or captured samples for:

- quiet room
- fan/AC noise
- keyboard/mouse noise
- desk bumps
- electrical hum
- distant background conversation
- user speech at normal volume
- user speech at low volume
- user speech starting after silence
- short word endings

Compare:

- passthrough
- WebRTC APM NS only
- RNNoise
- DeepFilterNet

Listen for:

- speech clipping
- robotic artifacts
- pumping
- delayed attack
- chopped word endings
- residual hiss
- transient smearing
- CPU spikes

Decision gate:

- DeepFilterNet must be meaningfully better than RNNoise for important Wavis scenarios.
- If quality is only marginally better but runtime and packaging are much worse, do not ship it.

## Phase 7: Wire Experimental Selection Into App

Only after Phase 1 through Phase 6 pass:

1. Add a client setting for noise suppression backend:
   - `None`
   - `RNNoise`
   - `DeepFilterNet Experimental`
2. Keep RNNoise as default.
3. Keep the existing on/off denoise toggle behavior understandable.
4. If the selected backend fails to construct, show a clear client-side error and keep the previous working backend.
5. Do not silently fall back from DeepFilterNet to RNNoise without telling the user.
6. Make sure APM NS follows `is_custom_suppressor_active()`.

The UI should label DeepFilterNet as experimental until it has enough real-world validation.

## Phase 8: Required Verification Commands

Run these before considering the work done:

```powershell
cargo test -p wavis-client-shared --features real-backends
cargo clippy -p wavis-client-shared --features real-backends -- -D warnings
cargo test -p wavis-client-shared --features real-backends,deepfilternet-backend
cargo clippy -p wavis-client-shared --features real-backends,deepfilternet-backend -- -D warnings
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Run the manual harness:

```powershell
cargo run -p wavis-client-shared --features real-backends,deepfilternet-backend --example noise_suppression_measure
```

If the app UI is changed, also run the relevant GUI checks.

## Files To Know

Core implementation:

```text
clients/shared/src/denoise_filter.rs
clients/shared/src/webrtc_apm.rs
clients/shared/Cargo.toml
```

Manual measurement:

```text
clients/shared/examples/noise_suppression_measure.rs
```

Documentation:

```text
doc/noise_suppression.md
DEEPFILTERNET_NOISE_SUPPRESSION_NEXT_STEPS.md
```

Potential GUI wiring:

```text
clients/wavis-gui/src/features/settings/
clients/wavis-gui/src/features/voice/
clients/wavis-gui/src-tauri/src/media.rs
```

## Non-Goals

Do not introduce:

- GPL dependencies
- cloud noise suppression
- backend-side media processing
- browser-only APIs
- GPU-only inference
- hidden model downloads at runtime
- silent fallback from DeepFilterNet to RNNoise
- stacked RNNoise + DeepFilterNet + WebRTC APM NS

## Recommended Next Task

The next concrete task should be:

```text
Create a feature-gated DeepFilterNet proof-of-concept in the manual measurement harness.
It should load a local model, process deterministic 48 kHz mono frames, and report startup/model-load cost plus average/max processing time per 20 ms frame. Do not wire it into the app UI yet.
```

That task gives a clear yes/no answer on whether DeepFilterNet is practical for Wavis before touching the live call path.
