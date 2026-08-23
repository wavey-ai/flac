# FLAC handoff and TODO

Last updated: 2026-08-22.

## Current state

The codec is at a good quality handoff point for native FLAC. The required
official decoder corpus, the native-FLAC files in the local FFmpeg FATE sample
tree, the retained Claxon fixtures, and deterministic cross-implementation
encoding checks are green. The largest known gaps are now performance,
continuous mutation fuzzing, end-of-stream integrity checks, and deliberately
unsupported exotic FLAC variants.

Do not optimize for one fixture. Keep codec changes general, put changes to the
inherited encoder or decoder in the corresponding fork first, and preserve the
sample-exact gates below.

### Quality results at handoff

- Official IETF/CELLAR required subset: **64/64** decoded sample-for-sample
  identically to both libFLAC and FFmpeg.
- Native-FLAC FATE files: **7/7** valid files decoded sample-exactly against
  both references.
- Retained Claxon valid fixtures: **6/6** sample-exact.
- Official uncommon group: 3 exotic files decoded exactly and 8 unsupported
  files rejected cleanly, as permitted by the corpus contract.
- Robustness group: 37 malformed/oversized/fuzz files, with 3 safely accepted,
  34 safely rejected, and **zero panics or hangs**.
- Combined external corpus summary: `files=125 compared=80
  optional_rejections=8 robustness_accepted=3 robustness_rejected=34
  failures=0`.
- Deterministic randomized differential run: **24/24 cases**, **192 exact
  Wavey-relevant encode/decode comparisons**, seed
  `6288714204092058689`, zero failures.
- Randomized geometry spans 8–384 kHz, 1–8 channels, signed 16/24-bit PCM,
  block sizes 32–16,384, short final blocks, extrema, impulses, low-amplitude
  values, ramps, and full-range pseudorandom samples.
- Westside whole-file quality remains exact on all seven previously recorded
  PCM paths. See `benchmarks/WESTSIDE_BASELINE.md`.

The formal local command is:

```sh
scripts/test-corpora.sh \
  /Users/jamie/wavey.ai/soundkit/build/flac-test-files \
  /Users/jamie/wavey.ai/soundkit/build/ffmpeg-fate-suite \
  24
```

The two external roots are intentionally not committed. The official corpus
checkout used here was `ietf-wg-cellar/flac-test-files` at
`aa7b0c6cf32994c106ae517a08134c28a96ff5b2`. The local FATE tree is about
1.3 GiB. The harness requires `flac` and `ffmpeg`; `sndfile-convert` is an
optional fallback oracle for an accepted exotic stream that libFLAC rejects.

`examples/corpus_check.rs` drives the real incremental decoder with varying
compressed chunk sizes, including one-byte input, and compares integer PCM
against libFLAC and FFmpeg. `examples/randomized_differential.rs` encodes every
generated case with Wavey balanced, Wavey maximum, FFmpeg, and libFLAC. Wavey
decodes all four streams; both reference decoders decode both Wavey streams.

The FATE Ogg-FLAC and Matroska-FLAC samples are not included because this crate
owns the native FLAC codec, not those container demuxers. The FATE
`cover_art/cover_art.flac` fixture is malformed as audio: libFLAC loses sync
and FFmpeg reports an invalid residual. It is correctly in the robustness set.

## Highest-priority remaining work

### 1. Close the measured FFmpeg performance gaps

Use multiple files and alternating A/B runs on a cool machine. Every retained
optimization must keep the corpus and randomized differential gates green.

The initial Apple M1 Westside baseline is:

| Path | Wavey median | FFmpeg median | Current gap |
|---|---:|---:|---:|
| Balanced encode | 1.191 s | 0.782 s | Wavey 1.52x slower; output 6.50% larger |
| Maximum encode | 2.157 s | 0.782 s | Wavey 2.76x slower; output 0.71% smaller |
| Decode FFmpeg level-5 FLAC | 0.426 s | 0.264 s | Wavey 1.61x slower |
| Decode Wavey FLAC | 0.480–0.586 s | 0.205–0.265 s | Wavey 2.21–2.34x slower |

Suggested next pass:

1. Re-establish the baseline with `scripts/bench-ffmpeg.sh`, then add at least
   mono/stereo, 16/24-bit, speech/music/noise, and short/long inputs before
   accepting a result.
2. Profile decoder bit input, unary/Rice decoding, LPC reconstruction, CRC,
   planar-to-interleaved copying, and stream-buffer compaction. Measure first;
   these are candidates, not established bottlenecks.
3. Profile encoder Rice/LPC search, frame-buffer work, MD5, and allocations in
   both balanced and maximum profiles. Compare matched block sizes separately
   from production defaults; FFmpeg used 4,608 samples while Wavey used 4,096
   in the recorded baseline.
 4. Keep improvements in `wavey-ai/claxon` or `wavey-ai/flacenc-rs` first so
    they remain suitable for contribution upstream, then integrate them here.
    Port status for the slice-reader round: the bulk slicing-by-eight CRC-16
    now exists in `wavey-ai/claxon` as the public `crc16_flac` with vector and
    reader-equivalence tests. The other changes are specific to the slice
    decode path and have no upstream counterpart yet: the full-window
    BitReader refill, the ghost-bit consume guards, and the Rice selection
    knob all depend on the 64-bit cache reader that upstream does not have.
    They stay here until an upstream design adopts a cached bit reader or a
    post-hoc frame checksum.
 5. Reject complexity that only helps Westside or one predictor/Rice
   distribution. Several such ideas were already measured and reverted during
   the first pass.

#### Slice-reader result (updated 2026-08-23)

Slice decoding is now the production path. Set `WAVEY_FLAC_STREAMING_DECODE=1`
to select the older streaming reader as an escape hatch.

The 2026-08-22 entry recorded three defects on this path. All three are fixed:

1. A release-mode shift bug corrupted one bit when a unary symbol spanned the
   full 64-bit cache window. Rust masks such shifts to a no-op in release
   builds, so one consumed bit survived below the unconsumed region. The next
   cache fill then mixed it into fresh data. Both consume sites now assign
   zero instead of shifting by the full width. The regression test
   `verify_unary_across_full_cache_refill_no_ghost_bits` covers this case.
2. The invalid subframe header came from the same ghost-bit corruption class
   and no longer reproduces.
3. The frame CRC mismatch had the same root cause.

Three further changes made the path faster than streaming. The environment
scan for the Rice knob moved into a once-per-process lookup. The bulk CRC-16
now uses slicing-by-8 with eight 256-entry columns. The slice reader refills
with one unaligned 8-byte load instead of a variable-length copy.

The plain Rice read measures faster than the fused port on Apple silicon, so
it is the default. Set `WAVEY_FLAC_RICE_FUSED=1` to select the fused
single-lookup form for comparison.

Benchmark matrix on the SoundKit CPU host `yl-encodec-1` (Intel Xeon
Platinum 8581C, 4 vCPU, Debian 12, rustc 1.97.1, libFLAC 1.4.2, idle load).
Each cell is a median-of-medians over three alternating rounds of seven
timed runs; checksums stay enabled. The slice path beats streaming on every
fixture. The gap to libFLAC stays between 1.6x and 2.5x:

```text
fixture              slice   stream  libflac   slice/libflac
blocksize 4608       0.0092  0.0100  0.0037    2.49
blocksize 512        0.0093  0.0101  0.0038    2.45
variable blocksize   0.0344  0.0358  0.0159    2.16
high resolution      0.0271  0.0286  0.0109    2.49
samplerate 384kHz    0.0989  0.1069  0.0427    2.32
mono audio (short)   0.0008  0.0009  0.0005    1.60
ffmpeg-level-5       0.2991  0.3272  0.1288    2.32
wavey-balanced       0.1874  0.2179  0.1137    1.65
wavey-maximum        0.2985  0.3260  0.1393    2.14
```

Tools: `examples/codec_bench.rs` for the Rust paths,
`scripts/libflac_bench.c` against system libFLAC, and
`scripts/bench_interleaved.py` plus the matrix driver used on the host for
alternating runs.

Retain `examples/codec_bench.rs`, `examples/decode_path_ab.rs`,
`examples/diff_stream_vs_flacreader.rs`, `examples/replay_diff.rs`, and the
`op_trace` module. They are the measurement and correctness tools for the
next decoder pass.

A hand-written NEON dot product for `predict_lpc_low_order` was also measured
and reverted. Both a single-accumulator and a three-accumulator version ran
about twice as slow as the scalar loop on Apple M1. The scalar loop keeps the
twelve-sample window in registers. A vector version must reload history that
the previous iteration just stored, and the store-forwarding stalls cost more
than the vector multiplies save.

### 2. Add decoder MD5 and total-sample verification

Frame/header CRCs are checked, but the incremental decoder currently parses
the STREAMINFO MD5 and total-sample count without verifying either at
`Decoder::finish()`. Add streaming PCM MD5 accumulation and decoded-frame
counting. A zero digest or zero total count means “unknown” and must remain
accepted. This would turn two currently safe-accepted faulty-corpus cases into
explicit integrity failures.

Also decide whether to enforce declared min/max frame sizes. The faulty corpus
currently safely accepts an incorrect maximum frame-size declaration. This is
metadata strictness, not a known PCM correctness defect.

### 3. Expand fuzzing beyond the 23 retained regressions

The checked-in Claxon fuzz corpus is useful regression coverage but is small.
Add a `cargo-fuzz` target around `stream::Decoder` that:

- feeds arbitrary bytes in arbitrary chunk boundaries;
- varies output buffer sizes, including zero/one sample;
- calls `finish()` and `reset()` in valid state transitions;
- asserts no panic, unbounded allocation, or hang;
- seeds from the 64 official subset files, 11 faulty files, 11 uncommon files,
  8 Claxon fixtures, 23 Claxon fuzz regressions, and 8 native-FLAC FATE files.

Keep newly minimized crashes as small checked-in regression files. Run a real
multi-hour mutation campaign; the deterministic generator is differential
property coverage, not a replacement for mutation fuzzing.

### 4. Decide optional format scope explicitly

The fixed-geometry streaming API deliberately rejects midstream changes to
sample rate, channel count, or bit depth. It also rejects 32-bit FLAC,
sample rates above 655,350 Hz, and native-frame streams without a `fLaC`
marker/STREAMINFO. Those official “uncommon” files are valid FLAC but clean
rejection is currently the intended production policy.

The encoder accepts 16-bit and 24-bit PCM. The decoder additionally covers
8/12/15/20-bit corpus files. Supporting more encoder depths is a feature gap,
not a known losslessness defect in the advertised 16/24-bit encoder.

If Ogg FLAC or Matroska FLAC is required, implement/test that in the SoundKit
container layer rather than adding a demuxer to this codec crate.

## Fork provenance for this pass

- `wavey-ai/claxon` `main`:
  - `58b984e` narrows Rice2 remainder readers;
  - `a55791c` interprets the frame-header 8-bit sample-rate field as kHz;
  - `71b53f7` decodes escaped Rice/Rice2 residual partitions, including
    zero-width escapes.
- `wavey-ai/flacenc-rs` `main`:
  - `ab2d6a1` batches PCM-byte MD5 updates;
  - `89bed7d` removes the inherited 96 kHz verifier ceiling and exercises the
    FLAC frame-header range up to 655,350 Hz.

The unified crate contains the corresponding source changes. Continue using
the forks' `main` branches rather than pinning SoundKit to one-off revisions.

## Useful observations

- Fixing the corpus harness mattered: libFLAC emits WAVE_FORMAT_EXTENSIBLE for
  12/20-bit PCM, and a stdout WAV cannot report an unknown total length. The
  checked-in harness decodes to a seekable temporary WAV and parses valid bits
  correctly.
- The official corpus directly exposed two real Claxon omissions: escaped Rice
  partitions and the kHz multiplier in frame headers. Both are now fixed in
  the fork and unified crate.
- The randomized suite exposed the inherited flacenc 96 kHz ceiling. The same
  seed now passes at 192/384 kHz.
- FFmpeg 8.1.2 returned zero samples, with a successful process exit, when the
  test harness asked it to decode its own generated 192 kHz/8-channel/24-bit
  case 23. Wavey decoded that FFmpeg file exactly. Reference-self decoding is
  therefore not part of the Wavey gate; only paths that exercise Wavey's
  encoder or decoder are counted.
- Do not spend the next pass on CI/FATE download plumbing or iOS. The requested
  scope is this FLAC codec, correctness, and then measured codec performance.
