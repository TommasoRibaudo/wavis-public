# Noise Suppression

Wavis currently uses RNNoise through `nnnoiseless` as the default custom
noise suppressor. It is cheap, CPU-only, already integrated in the native
capture path, and has been validated with the current 48 kHz / 20 ms audio
pipeline.

The capture-side suppression chain must not stack multiple denoisers. Use one
of RNNoise, a future DeepFilterNet backend, or WebRTC APM noise suppression.
When a custom suppressor is active, WebRTC APM NS is disabled so the pipeline
does not double-suppress speech.

## Backend Model

`DenoiseFilter` is a small backend host. `DenoiseFilter::new(enabled)` remains
infallible and selects RNNoise. Explicit construction is available through:

- `DenoiseFilter::with_infallible_backend(InfallibleNoiseSuppressorKind, enabled)`
- `DenoiseFilter::try_with_backend(NoiseSuppressorKind, enabled)`

`NoiseSuppressorKind::None` is a passthrough backend. Its raw user toggle may
be enabled, but it is not an active custom suppressor. Use
`is_custom_suppressor_active()` for APM coordination, not `is_enabled()`.

## DeepFilterNet Status

DeepFilterNet2 was the preferred first research-backed candidate because it is
designed for speech enhancement at 48 kHz, but the current upstream Rust
runtime is DFN3-oriented. Upstream `DfTract` explicitly rejects DFN2 models and
points older DFN2 users to older `v0.3.1` code. The current Wavis experimental
path therefore uses DeepFilterNet3 through upstream `libDF` pinned to the
`v0.5.6` release tag.

The `deepfilternet-backend` Cargo feature now builds a native CPU-only
DeepFilterNet proof of concept. It uses upstream `deep_filter` with Tract and
the embedded default DFN3 model, keeps model/runtime state inside a dedicated
worker thread because `DfTract` is not `Send`, and accepts the Wavis public
960-sample / 48 kHz mono frame contract. Construction errors remain explicit
through `DenoiseFilter::try_with_backend(...)`; there is no silent fallback to
RNNoise.

DeepFilterNet is still experimental. It is available through the manual
measurement harness and internal construction APIs only; the GUI remains a
single noise suppression on/off toggle.

Real-audio Phase 6 listening is currently `Needs work`, not `Pass`.
DeepFilterNet reduced noise better than RNNoise in some cases, but the human
listening pass reported an echo/layered voice artifact. Phase 6.5 is focused
on fixing or proving that artifact before any app wiring. The backend now
compensates the algorithmic delay reported by upstream DeepFilterNet
metadata: `(fft_size - hop_size) + (lookahead * hop_size)`. The embedded DFN3
model currently reports `latency_samples=1440` at 48 kHz.

Do not add GPL components, cloud processing, GPU-only paths, browser APIs, or
backend-side media processing for noise suppression.

## Deferred Options

PercepNet, FullSubNet, Demucs, and target-speaker enhancement are deferred.
The risks are latency, runtime complexity, dependency/licensing fit, model
packaging, and product fit for a small native real-time voice app.

Generic denoise will not reliably remove competing voices. Removing another
speaker while preserving the target speaker requires target-speaker
enhancement, enrollment, or mic-array/beamforming techniques. Treat
third-party voice suppression as a future voice-isolation research track, not
as a denoise-only requirement.

## Measurement

Run the automated gates:

```powershell
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo test -p wavis-client-shared --features real-backends
cargo clippy -p wavis-client-shared --features real-backends -- -D warnings
```

If touching the DeepFilterNet scaffold, also run:

```powershell
cargo check -p wavis-client-shared --features real-backends,deepfilternet-backend
cargo test -p wavis-client-shared --features real-backends,deepfilternet-backend
cargo clippy -p wavis-client-shared --features real-backends,deepfilternet-backend -- -D warnings
```

Manual timing harness:

```powershell
cargo run -p wavis-client-shared --features real-backends,deepfilternet-backend --example noise_suppression_measure
cargo run -p wavis-client-shared --release --features real-backends,deepfilternet-backend --example noise_suppression_measure
```

The harness uses deterministic synthetic inputs: silence, white noise,
pink-ish noise, hum plus noise, impulses, speech-like signal, speech starting
after silence, speech plus clicks, short words, quiet voice, and two mixed
speech-like signals. It runs passthrough, RNNoise, and
DeepFilterNetExperimental, then reports backend name, startup/model-load time,
algorithmic latency, average/max processing time per 20 ms frame, output RMS,
finite-output status, output length, and peak-offset diagnostics.

Current release harness result on this Windows workstation: DeepFilterNet
startup/model load is about 289 ms; synthetic non-silent cases average about
0.7-2.6 ms per 20 ms frame with max spikes below about 4.1 ms. Debug builds are
much slower and should not be used for real-time viability decisions.
