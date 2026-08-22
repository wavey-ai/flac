# Westside whole-file FLAC baseline

This is the initial production-shaped baseline for Wavey's unified Rust FLAC
codec. Correctness is a hard gate: no timing is accepted unless every native
and cross-decoder path reproduces FFmpeg's canonical PCM byte for byte.

## Fixture and environment

- Date: 2026-08-22
- Source: `WESTSIDE_MIX 4 CONFIRMATION_130323.wav`
- Source SHA-256: `dec1b383d58aea9848728126efab169f42e54375b64ca55363d2f234696474c9`
- Source MD5: `e23438d6c6b8d8d3cc5f423cf3a32aac`
- Canonical PCM MD5: `f422b8b37c1fddb9f5c4c40ef68e305b`
- Geometry: 48,000 Hz, stereo, signed 24-bit PCM, 9,389,979 frames
- Duration: 195.624562 seconds
- Decoded size: 56,339,874 bytes
- Machine: MacBookAir10,1, Apple M1, arm64
- OS: macOS 26.5 (25F71)
- Rust: 1.96.0
- FFmpeg: 8.1.2

The Wavey encoders used 4,096-sample blocks: 2,292 full blocks and one
1,947-sample final block. FFmpeg level 5 selected its normal 4,608-sample
blocks: 2,037 full blocks and one 3,483-sample final block. This first result
therefore compares the intended production settings rather than forcing a
matched-block microbenchmark.

## Encode baseline

| Encoder | Median | Minimum | Speed | FLAC bytes | Bitrate | PCM size |
|---|---:|---:|---:|---:|---:|---:|
| Wavey balanced | 1.191 s | 1.170 s | 164.2x realtime | 43,084,252 | 1,761.9 kb/s | 76.47% |
| Wavey maximum | 2.157 s | 1.777 s | 90.7x realtime | 40,168,395 | 1,642.7 kb/s | 71.30% |
| FFmpeg level 5 | 0.782 s | 0.667 s | 250.2x realtime | 40,455,835 | 1,654.4 kb/s | 71.81% |

Wavey maximum produced a 287,440-byte (0.71%) smaller file than FFmpeg level
5, but FFmpeg encoded 2.76x faster by median time. Wavey balanced encoded 1.52x
slower than FFmpeg and produced a 6.50% larger file.

## Decode baseline

Both decoders were timed against every encoded stream. Comparing a row pair
keeps compressed input identical.

| Decoder | FLAC input | Median | Minimum | Speed | Quality |
|---|---|---:|---:|---:|---|
| Wavey | Wavey balanced | 0.586 s | 0.547 s | 333.9x realtime | bit-exact |
| FFmpeg | Wavey balanced | 0.265 s | 0.209 s | 737.9x realtime | bit-exact |
| Wavey | Wavey maximum | 0.480 s | 0.455 s | 407.5x realtime | bit-exact |
| FFmpeg | Wavey maximum | 0.205 s | 0.196 s | 954.3x realtime | bit-exact |
| Wavey | FFmpeg level 5 | 0.426 s | 0.424 s | 459.6x realtime | bit-exact |
| FFmpeg | FFmpeg level 5 | 0.264 s | 0.231 s | 741.4x realtime | bit-exact |

On the same FFmpeg level-5 file, Wavey's decoder is currently 1.61x slower.
Across the two Wavey streams, it is 2.21–2.34x slower than FFmpeg.

## First optimization pass

Westside is a production-shaped performance fixture, not a source of codec
special cases. Changes from this pass were accepted only when they apply to
the FLAC format generally and preserve exact output. No decision depends on
this file's LPC-order distribution, Rice-parameter range, stereo content, or
signal characteristics.

| Layer | Commit | General change | Validation |
|---|---|---|---|
| `flacenc-rs` fork | `ab2d6a1` | Reuse one PCM byte buffer and update the stream MD5 once per frame instead of once per sample. | The maximum-profile FLAC remained byte-identical: 40,168,395 bytes, SHA-256 `5007250bae0a11c18059e45071fbd01784f3a2532888ee7329b476ae3c8cc29c`. Alternating hot runs indicated roughly 5–10% lower encode time. |
| Claxon fork | `58b984e` | Decode Rice2 remainders with the narrowest valid 8-, 16-, or 32-bit reader. | Parameters 0, 8, 9, 16, 17, and 30 are covered; the full FFmpeg level-5 stream decodes bit-exactly. |
| Unified stream API | `b17a9a9` | Reuse decoded-frame storage and specialize whole-block copying without changing the codec algorithm. | Whole-file A/B medians improved 3.6–12.1% across Wavey balanced, Wavey maximum, and FFmpeg level-5 inputs; mono, stereo, 3-channel, and 8-channel paths are covered. |

Absolute A/B times during this pass were affected by fanless-laptop thermal
throttling, so only alternating or same-session relative deltas are reported.
Every retained path passed the quality gate below. An LPC-order specialization,
a Rice-search recurrence, a sample-range shortcut, and an alternative byte
packing loop were measured and rejected rather than retained as fixture-tuned
complexity.

## Quality gate

All seven independently produced PCM paths contained exactly 56,339,874 bytes
with MD5 `f422b8b37c1fddb9f5c4c40ef68e305b`:

| Path | Result |
|---|---|
| Wavey WAV input | bit-exact to FFmpeg WAV input |
| Wavey decoder <- Wavey balanced | bit-exact |
| FFmpeg decoder <- Wavey balanced | bit-exact |
| Wavey decoder <- Wavey maximum | bit-exact |
| FFmpeg decoder <- Wavey maximum | bit-exact |
| Wavey decoder <- FFmpeg level 5 | bit-exact |
| FFmpeg decoder <- FFmpeg level 5 | bit-exact |

The maximum absolute sample error is 0, the mismatched-sample count is 0, and
the signal-to-error ratio is infinite. This establishes both encoder
losslessness and interoperability in both directions.

## Method

Run from the repository root:

```sh
scripts/bench-ffmpeg.sh /path/to/WESTSIDE_MIX.wav 5 target/westside-baseline
```

The harness performs one untimed warm-up followed by five measured iterations
and reports their median and minimum. Timings include source reads, output
writes, WAV/FLAC framing, PCM packing, and (for FFmpeg) process startup. They do
not call `fsync`, pin CPU cores, or suppress normal laptop thermal and scheduler
variation. FFmpeg is explicitly invoked with `-compression_level 5`; Wavey is
single-threaded. All generated artifacts remain under ignored `target/` paths.

The benchmark exits nonzero on a length mismatch or the first differing PCM
byte, before reporting a successful quality result.
