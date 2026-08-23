//! Differential check: stream::Decoder output must match the generic
//! FrameReader-based FlacReader path sample for sample.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --example diff_stream_vs_flacreader -- FILE.flac
//! ```

use std::env;
use std::error::Error;
use std::fs;

use wavey_flac::decoder::FlacReader;
use wavey_flac::stream::Decoder;

type AnyError = Box<dyn Error>;

const INPUT_CHUNK_BYTES: usize = 1024 * 1024;
const OUTPUT_CHUNK_SAMPLES: usize = 256 * 1024;

fn main() -> Result<(), AnyError> {
    let flac_path = env::args().nth(1).expect("usage: diff FILE.flac");
    let flac = fs::read(&flac_path)?;

    // Reference pass: the unchanged generic reader.
    let (info, samples): (_, Vec<i32>) = {
        let mut freader = FlacReader::new(&flac[..])?;
        let info = freader.streaminfo();
        let mut samples = Vec::<i32>::new();
        for sample in freader.samples() {
            samples.push(sample?);
        }
        (info, samples)
    };

    // Candidate pass: the incremental slice-backed decoder, fed exactly like
    // production feeds it.
    let mut decoder = Decoder::new();
    let mut output = vec![0_i32; OUTPUT_CHUNK_SAMPLES];
    let mut candidate = Vec::<i32>::with_capacity(samples.len());
    for chunk in flac.chunks(INPUT_CHUNK_BYTES) {
        let written = decoder.decode_i32(chunk, &mut output)?;
        candidate.extend_from_slice(&output[..written]);
        loop {
            let written = decoder.decode_i32(&[], &mut output)?;
            if written == 0 {
                break;
            }
            candidate.extend_from_slice(&output[..written]);
        }
    }
    loop {
        let written = decoder.decode_i32(&[], &mut output)?;
        if written == 0 {
            break;
        }
        candidate.extend_from_slice(&output[..written]);
    }
    decoder.finish()?;

    assert_eq!(
        candidate.len(),
        samples.len(),
        "sample count mismatch: stream decoder {} vs flacreader {}",
        candidate.len(),
        samples.len()
    );
    for (i, (a, b)) in candidate.iter().zip(samples.iter()).enumerate() {
        if a != b {
            return Err(format!(
                "sample mismatch at {}: stream decoder {}, flacreader {}",
                i, a, b
            )
            .into());
        }
    }

    println!(
        "OK: {} samples identical ({}, {} Hz, {} ch, {} bps)",
        candidate.len(),
        flac_path,
        info.sample_rate,
        info.channels,
        info.bits_per_sample
    );
    Ok(())
}
