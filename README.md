# Wavey FLAC

Wavey FLAC is one pure-Rust project for fast FLAC encoding and decoding. It
combines the performance-focused `flacenc-rs` encoder with the mature Claxon
decoder behind one crate and one streaming API.

The project currently provides:

- incremental native-FLAC decoding from arbitrarily sized byte chunks;
- frame-oriented 16-bit and 24-bit encoding for 1–8 channels;
- raw FLAC frame encoding and decoding for packet transports;
- metadata parsing, CRC validation, corruption limits, and exact round-trip
  tests in the same crate;
- a final `STREAMINFO` header for file or container backpatching.

```rust
use wavey_flac::frame::{FlacFrameConfig, FlacProfile};
use wavey_flac::stream::{Decoder, Encoder};

let config = FlacFrameConfig::new(48_000, 2, 16, 128, FlacProfile::Balanced)?;
let mut encoder = Encoder::new(config)?;
let samples = vec![0_i32; 128 * 2];
let mut encoded = Vec::new();
encoder.encode_i32(&samples, &mut encoded)?;

// Native files begin with this marker. `encoded` begins with STREAMINFO.
let mut file = b"fLaC".to_vec();
file.extend_from_slice(&encoded);

let mut decoder = Decoder::new();
let mut decoded = vec![0_i32; samples.len()];
let written = decoder.decode_i32(&file, &mut decoded)?;
assert_eq!(&decoded[..written], samples);
# Ok::<(), wavey_flac::frame::FlacFrameError>(())
```

For more than one frame, call `Encoder::finish()` and replace the metadata at
the start of a seekable file with that finalized header. A non-seekable stream
remains valid with unknown total samples and an unset MD5 digest.

## Origins

The Git graph preserves both source histories. Encoder work descends from
[`yotarok/flacenc-rs`](https://github.com/yotarok/flacenc-rs); decoder work
descends from [`ruuda/claxon`](https://github.com/ruuda/claxon). Both were
Apache-2.0 projects. See [THIRD_PARTY.md](THIRD_PARTY.md) and the retained
upstream documentation under [`docs/`](docs/).

## Validation

```sh
cargo test
cargo test --no-default-features
```

`simd-nightly` deliberately requires a nightly toolchain, so it is tested
separately rather than through `--all-features` on stable Rust.

The whole-file FFmpeg baseline is both a benchmark and a losslessness gate. It
encodes with Wavey's balanced and maximum profiles plus FFmpeg level 5, decodes
all three files with both implementations, and rejects any PCM byte mismatch:

```sh
scripts/bench-ffmpeg.sh path/to/input-24-bit.wav 5
```

Set `FFMPEG` to select a non-default binary. Generated FLAC and PCM artifacts
stay under the ignored `target/` directory. See the checked-in
[Westside baseline](benchmarks/WESTSIDE_BASELINE.md) for the initial result and
methodology.

The broader acceptance gate is byte-exact decoding against libFLAC and FFmpeg
across the upstream FLAC/FATE corpora.

## License

Apache-2.0. See [LICENSE](LICENSE) and [LICENSE-CLAXON](LICENSE-CLAXON).
