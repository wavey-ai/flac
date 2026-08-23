//! Differential harness: replays a dumped frame body through
//! both bit readers at many start-bit offsets and reports any divergence.

use std::io::Cursor;

use wavey_flac::decoder::input::{op_trace, BitReader, Bitstream, Traced};
use wavey_flac::decoder::subframe;

fn decode_all<S: wavey_flac::decoder::input::BitSource>(
    input: &mut S,
    channels: usize,
    bps: u32,
) -> Result<(), String> {
    let mut buf = vec![0_i32; 16384];
    for _ in 0..channels {
        let mut t = Traced::new(input);
        subframe::decode(&mut t, bps, &mut buf).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let body = std::fs::read(&args[1]).expect("frame body");
    let max_offsets: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    assert!(
        (1..=8).contains(&max_offsets),
        "offset count must be between 1 and 8"
    );
    let channels: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
    let bps: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(24);

    for skip_bits in 0..max_offsets {
        // Pre-shift a copy so both readers start at a clean byte boundary.
        let shifted: Vec<u8> = if skip_bits == 0 {
            body.clone()
        } else {
            let bit = skip_bits;
            let mut shifted = Vec::with_capacity(body.len());
            let mut acc = 0_u8;
            for &byte in &body {
                shifted.push(acc | (byte >> bit));
                acc = byte << (8 - bit);
            }
            shifted.push(acc >> (8 - bit));
            shifted
        };

        op_trace::install();
        let stream_result = {
            let cursor = Cursor::new(shifted.clone());
            let mut stream = Bitstream::new(cursor);
            decode_all(&mut stream, channels, bps)
        };
        let ops_a = op_trace::take_all();
        op_trace::uninstall();

        op_trace::install();
        let slice_result = {
            let mut reader = BitReader::new(&shifted);
            let r = decode_all(&mut reader, channels, bps);
            r
        };
        let ops_b = op_trace::take_all();
        op_trace::uninstall();

        let structural_err = format!("{stream_result:?}") != format!("{slice_result:?}");
        let mut first_diff = None;
        let n = ops_a.len().min(ops_b.len());
        for i in 0..n {
            let (a, b) = (&ops_a[i], &ops_b[i]);
            if a.tag != b.tag || a.width != b.width || a.value != b.value {
                first_diff = Some(i);
                break;
            }
        }
        if first_diff.is_some() || structural_err || ops_a.len() != ops_b.len() {
            println!(
                "skip_bits={skip_bits}: stream={stream_result:?} slice={slice_result:?} lens=({},{})",
                ops_a.len(),
                ops_b.len()
            );
            if let Some(i) = first_diff {
                println!("  FIRST DIVERGENCE at op {i}");
                for j in i.saturating_sub(6)..=(i + 3).min(n - 1) {
                    println!(
                        "  stream op{j} {} w={} v={} pos={}",
                        ops_a[j].tag as char, ops_a[j].width, ops_a[j].value, ops_a[j].pos_before
                    );
                    println!(
                        "  slice  op{j} {} w={} v={} pos={}",
                        ops_b[j].tag as char, ops_b[j].width, ops_b[j].value, ops_b[j].pos_before
                    );
                }
            }
        }
    }
    println!("scan done");
}
