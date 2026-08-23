//! In-process A/B: the old Cursor+FrameReader decode path versus the new
//! slice-backed stream decoder, interleaved under identical machine load.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --example decode_path_ab -- FILE.flac [ROUNDS]
//! ```

use std::error::Error;
use std::fs;
use std::io::Cursor;
use std::time::{Duration, Instant};

use wavey_flac::decoder::frame::FrameReader;
use wavey_flac::decoder::FlacReader;
use wavey_flac::stream::Decoder;

type AnyError = Box<dyn Error>;

const INPUT_CHUNK_BYTES: usize = 1024 * 1024;
const OUTPUT_CHUNK_SAMPLES: usize = 256 * 1024;

fn median(samples: &[Duration]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1].as_secs_f64() + sorted[middle].as_secs_f64()) / 2.0
    } else {
        sorted[middle].as_secs_f64()
    }
}

/// Byte offset of the first audio frame: skips fLaC magic and all metadata
/// blocks.
fn audio_offset(flac: &[u8]) -> usize {
    assert_eq!(&flac[0..4], b"fLaC", "not a FLAC file");
    let mut pos = 4;
    loop {
        let header = &flac[pos..pos + 4];
        let is_last = header[0] & 0x80 != 0;
        let len = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
        pos += 4 + len;
        if is_last {
            return pos;
        }
    }
}

/// The pre-optimization path: FrameReader over a cursor, checksum computed
/// interleaved with every byte-level read.
fn decode_old(flac: &[u8], sink: &mut u64) -> Duration {
    let (sample_rate, bits_per_sample, channels) = {
        let reader = FlacReader::new(flac).expect("metadata");
        (
            reader.streaminfo().sample_rate,
            reader.streaminfo().bits_per_sample,
            reader.streaminfo().channels as u32,
        )
    };
    let started = Instant::now();
    let mut checksum = 0_u64;
    let mut rest = &flac[audio_offset(flac)..];
    loop {
        if rest.is_empty() {
            break;
        }
        let mut cursor = Cursor::new(rest);
        let mut frame_reader =
            FrameReader::with_stream_info(&mut cursor, sample_rate, bits_per_sample);
        match frame_reader.read_next_or_eof(Vec::new()) {
            Ok(Some(block)) => {
                debug_assert_eq!(block.channels(), channels);
                for s in block.stereo_samples() {
                    checksum ^= s.0 as u64 ^ s.1 as u64;
                }
                let consumed = cursor.position() as usize;
                rest = &rest[consumed..];
            }
            Ok(None) => break,
            Err(err) => panic!("old-path decode error: {err}"),
        }
        // Keep allocation behavior comparable to production: reuse nothing.
    }
    *sink ^= checksum;
    started.elapsed()
}

/// The new path: slice-backed bit reader plus bulk CRC verification.
fn decode_new(decoder: &mut Decoder, flac: &[u8], sink: &mut u64) -> Duration {
    let started = Instant::now();
    let mut output = vec![0_i32; OUTPUT_CHUNK_SAMPLES];
    let mut checksum = 0_u64;
    for chunk in flac.chunks(INPUT_CHUNK_BYTES) {
        let written = decoder.decode_i32(chunk, &mut output).expect("new path");
        for &sample in &output[..written] {
            checksum ^= sample as u64;
        }
        loop {
            let written = decoder.decode_i32(&[], &mut output).expect("new path");
            if written == 0 {
                break;
            }
            for &sample in &output[..written] {
                checksum ^= sample as u64;
            }
        }
    }
    loop {
        let written = decoder.decode_i32(&[], &mut output).expect("new path");
        if written == 0 {
            break;
        }
        for &sample in &output[..written] {
            checksum ^= sample as u64;
        }
    }
    decoder.finish().expect("finish");
    *sink ^= checksum;
    started.elapsed()
}

fn main() -> Result<(), AnyError> {
    let args: Vec<String> = std::env::args().collect();
    let flac_path = args
        .get(1)
        .ok_or("usage: decode_path_ab FILE.flac [ROUNDS]")?;
    let rounds: usize = args.get(2).map(|s| s.parse()).unwrap_or(Ok(5))?;
    let flac = fs::read(flac_path)?;

    // Warm-up both paths once and confirm they agree on a checksum.
    let mut sink_old = 0_u64;
    let mut sink_new = 0_u64;
    let mut fresh_decoder = Decoder::new();
    let warm_old = decode_old(&flac, &mut sink_old);
    let warm_new = decode_new(&mut fresh_decoder, &flac, &mut sink_new);
    assert_eq!(sink_old, sink_new, "paths disagree on output checksum");
    eprintln!(
        "warm-up: old {:.4} s, new {:.4} s",
        warm_old.as_secs_f64(),
        warm_new.as_secs_f64()
    );

    let mut timings_old = Vec::with_capacity(rounds);
    let mut timings_new = Vec::with_capacity(rounds);
    for round in 0..rounds {
        // Alternate execution order so systematic load spikes cannot always
        // penalize the same path.
        if round % 2 == 0 {
            timings_old.push(decode_old(&flac, &mut sink_old));
            let mut fresh_decoder = Decoder::new();
            timings_new.push(decode_new(&mut fresh_decoder, &flac, &mut sink_new));
        } else {
            let mut fresh_decoder = Decoder::new();
            timings_new.push(decode_new(&mut fresh_decoder, &flac, &mut sink_new));
            timings_old.push(decode_old(&flac, &mut sink_old));
        }
        assert_eq!(sink_old, sink_new, "paths disagree on output checksum");
        eprintln!(
            "round {round}: old {:.4} s, new {:.4} s",
            timings_old.last().unwrap().as_secs_f64(),
            timings_new.last().unwrap().as_secs_f64(),
        );
    }

    eprintln!(
        "old path: median {:.4} s, min {:.4} s over {rounds} runs",
        median(&timings_old),
        timings_old.iter().min().unwrap().as_secs_f64(),
    );
    eprintln!(
        "new path: median {:.4} s, min {:.4} s over {rounds} runs",
        median(&timings_new),
        timings_new.iter().min().unwrap().as_secs_f64(),
    );
    println!("OK");
    Ok(())
}
