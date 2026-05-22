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

DeepFilterNet2 is the preferred first research-backed candidate because it is
designed for speech enhancement at 48 kHz and upstream documents Windows,
macOS, and Linux support under MIT/Apache-2.0 licensing. Current Rust
integration is not settled: the available Rust APIs and upstream model-loading
paths are DFN3-oriented, while DFN2 compatibility and packaging still need a
clean cross-platform proof.

For now, the `deepfilternet-backend` Cargo feature is a scaffold only.
Constructing `DeepFilterNetExperimental` returns a clear error instead of
falling back to RNNoise. DeepFilterNet3 can be investigated after DFN2 if DFN2
remains impractical.

Do not add GPL components, cloud processing, GPU-only paths, browser APIs, or
backend-side media processing for noise suppression.

## Deferred Options

PercepNet, FullSubNet, Demucs, and target-speaker enhancement are deferred.
The risks are latency, runtime complexity, dependency/licensing fit, model
packaging, and product fit for a small native real-time voice app.

Generic denoise will not reliably remove competing voices. Removing another
speaker while preserving the target speaker requires target-speaker
enhancement, enrollment, or mic-array/beamforming techniques.

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
cargo test -p wavis-client-shared --features real-backends,deepfilternet-backend
cargo clippy -p wavis-client-shared --features real-backends,deepfilternet-backend -- -D warnings
```

Manual timing harness:

```powershell
cargo run -p wavis-client-shared --features real-backends --example noise_suppression_measure
```

The harness uses deterministic synthetic inputs: silence, white noise,
pink-ish noise, hum plus noise, impulses, speech-like signal, speech plus
clicks, and two mixed speech-like signals. It reports startup construction
cost and average/max processing time per 20 ms frame. If DeepFilterNet becomes
real, extend the same harness with model-load timing.
