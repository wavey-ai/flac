//! Deterministic randomized FLAC cross-implementation quality gate.
//!
//! Every generated PCM case is encoded by Wavey (balanced and maximum),
//! FFmpeg, and libFLAC. Wavey decodes all four results; FFmpeg and libFLAC
//! additionally decode both Wavey results. Every path is compared
//! sample-for-sample with the source.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --example randomized_differential -- [CASES] [SEED] [START]
//! ```

use hound::{SampleFormat, WavSpec, WavWriter};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use wavey_flac::frame::{FlacFrameConfig, FlacProfile};
use wavey_flac::stream::{Decoder, Encoder};

type AnyError = Box<dyn Error>;

const DEFAULT_CASES: usize = 24;
const DEFAULT_SEED: u64 = 6_288_714_204_092_058_689;
const INPUT_CHUNKS: [usize; 8] = [1, 2, 7, 251, 4_093, 65_536, 17, 1_048_576];
const OUTPUT_SAMPLES: usize = 4_099;

#[derive(Clone, Copy, Debug)]
struct Case {
    index: usize,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u8,
    block_size: u32,
    frames: usize,
}

struct Asset {
    label: &'static str,
    path: PathBuf,
    wavey_encoded: bool,
}

fn main() -> Result<(), AnyError> {
    let mut arguments = env::args().skip(1);
    let cases = arguments
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(DEFAULT_CASES);
    let seed = arguments
        .next()
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(DEFAULT_SEED);
    let start = arguments
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    if cases == 0 || arguments.next().is_some() {
        return Err(
            "usage: randomized_differential [positive CASES] [decimal SEED] [START]".into(),
        );
    }

    let ffmpeg = env::var_os("FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let flac = env::var_os("FLAC").unwrap_or_else(|| "flac".into());
    ensure_tool(&ffmpeg)?;
    ensure_tool(&flac)?;

    let directory = tempfile::tempdir()?;
    let mut comparisons = 0usize;
    let end = start.checked_add(cases).ok_or("case range overflow")?;
    for index in start..end {
        let case = make_case(index);
        let samples = generate_samples(case, seed);
        let wav = directory.path().join(format!("case-{index}.wav"));
        write_wav(&wav, case, &samples)?;

        let balanced = directory
            .path()
            .join(format!("case-{index}-wavey-balanced.flac"));
        fs::write(
            &balanced,
            wavey_encode(case, &samples, FlacProfile::Balanced)?,
        )?;
        let maximum = directory
            .path()
            .join(format!("case-{index}-wavey-maximum.flac"));
        fs::write(
            &maximum,
            wavey_encode(case, &samples, FlacProfile::Maximum)?,
        )?;
        let ffmpeg_file = directory.path().join(format!("case-{index}-ffmpeg.flac"));
        ffmpeg_encode(&ffmpeg, &wav, &ffmpeg_file)?;
        let libflac_file = directory.path().join(format!("case-{index}-libflac.flac"));
        libflac_encode(&flac, &wav, &libflac_file)?;

        let assets = [
            Asset {
                label: "Wavey balanced",
                path: balanced,
                wavey_encoded: true,
            },
            Asset {
                label: "Wavey maximum",
                path: maximum,
                wavey_encoded: true,
            },
            Asset {
                label: "FFmpeg",
                path: ffmpeg_file,
                wavey_encoded: false,
            },
            Asset {
                label: "libFLAC",
                path: libflac_file,
                wavey_encoded: false,
            },
        ];

        for asset in &assets {
            let wavey = wavey_decode(&asset.path, case)?;
            require_samples(case, &samples, &wavey, asset.label, "Wavey")?;
            comparisons += 1;

            // Reference decoders validate Wavey's encoder. Running reference
            // decoders on reference-encoded files does not exercise Wavey.
            if asset.wavey_encoded {
                let ffmpeg_pcm = ffmpeg_decode(&ffmpeg, &asset.path, case, directory.path())?;
                require_samples(case, &samples, &ffmpeg_pcm, asset.label, "FFmpeg")?;
                comparisons += 1;

                let libflac_pcm = libflac_decode(&flac, &asset.path, case, directory.path())?;
                require_samples(case, &samples, &libflac_pcm, asset.label, "libFLAC")?;
                comparisons += 1;
            }
        }

        println!(
            "PASS case={} seed={} {}Hz {}ch {}bit block={} frames={} comparisons={}",
            case.index,
            seed,
            case.sample_rate,
            case.channels,
            case.bits_per_sample,
            case.block_size,
            case.frames,
            assets.len() + assets.iter().filter(|asset| asset.wavey_encoded).count() * 2
        );
    }

    println!(
        "SUMMARY cases={cases} start={start} seed={seed} exact_comparisons={comparisons} failures=0"
    );
    Ok(())
}

fn make_case(index: usize) -> Case {
    const SAMPLE_RATES: [u32; 12] = [
        8_000, 11_025, 16_000, 22_050, 32_000, 39_000, 44_100, 48_000, 88_200, 96_000, 192_000,
        384_000,
    ];
    const BLOCK_SIZES: [u32; 8] = [32, 192, 576, 1_000, 1_024, 4_096, 4_608, 16_384];

    let block_size = BLOCK_SIZES[(index * 5 + 3) % BLOCK_SIZES.len()];
    let tail = match index % 4 {
        0 => 0,
        1 => 32,
        2 => 37,
        _ => usize::try_from(block_size / 3).unwrap().max(32),
    };
    Case {
        index,
        sample_rate: SAMPLE_RATES[(index * 7 + 5) % SAMPLE_RATES.len()],
        channels: (index % 8 + 1) as u16,
        bits_per_sample: if index.is_multiple_of(2) { 16 } else { 24 },
        block_size,
        frames: usize::try_from(block_size).unwrap() * (index % 3 + 1) + tail,
    }
}

fn generate_samples(case: Case, seed: u64) -> Vec<i32> {
    let case_seed = seed ^ (case.index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut rng = StdRng::seed_from_u64(case_seed);
    let minimum = -(1_i32 << (case.bits_per_sample - 1));
    let maximum = (1_i32 << (case.bits_per_sample - 1)) - 1;
    let channels = usize::from(case.channels);
    let mut samples = Vec::with_capacity(case.frames * channels);

    for frame in 0..case.frames {
        for channel in 0..channels {
            let random = rng.gen_range(minimum..=maximum);
            let value = match (frame + channel * 11 + case.index) % 13 {
                0 => 0,
                1 => minimum,
                2 => maximum,
                3 => -1,
                4 => 1,
                5 => random >> 8,
                6 => {
                    ((frame as i64 * 257 + channel as i64 * 65_537) as i32).clamp(minimum, maximum)
                }
                7 if frame % 257 == 0 => maximum,
                8 if frame % 263 == 0 => minimum,
                _ => random,
            };
            samples.push(value);
        }
    }
    samples
}

fn write_wav(path: &Path, case: Case, samples: &[i32]) -> Result<(), AnyError> {
    let spec = WavSpec {
        channels: case.channels,
        sample_rate: case.sample_rate,
        bits_per_sample: u16::from(case.bits_per_sample),
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}

fn wavey_encode(case: Case, samples: &[i32], profile: FlacProfile) -> Result<Vec<u8>, AnyError> {
    let config = FlacFrameConfig::new(
        case.sample_rate,
        case.channels,
        case.bits_per_sample,
        case.block_size,
        profile,
    )?;
    let mut encoder = Encoder::new(config)?;
    let header_len = encoder.stream_header().len();
    let samples_per_block = config.sample_count()?;
    let mut encoded = Vec::new();
    for block in samples.chunks(samples_per_block) {
        encoder.encode_i32(block, &mut encoded)?;
    }
    let final_header = encoder.finish()?.to_vec();
    if final_header.len() != header_len || encoded.len() < header_len {
        return Err("Wavey final STREAMINFO cannot be backpatched".into());
    }
    encoded[..header_len].copy_from_slice(&final_header);
    let mut file = Vec::with_capacity(4 + encoded.len());
    file.extend_from_slice(b"fLaC");
    file.extend_from_slice(&encoded);
    Ok(file)
}

fn wavey_decode(path: &Path, case: Case) -> Result<Vec<i32>, AnyError> {
    let input = fs::read(path)?;
    let mut decoder = Decoder::new();
    let mut output = vec![0_i32; OUTPUT_SAMPLES];
    let mut samples = Vec::new();
    let mut offset = 0usize;
    let mut chunk = 0usize;
    while offset < input.len() {
        let end = input
            .len()
            .min(offset + INPUT_CHUNKS[chunk % INPUT_CHUNKS.len()]);
        pump_wavey(&mut decoder, &input[offset..end], &mut output, &mut samples)?;
        offset = end;
        chunk += 1;
    }
    pump_wavey(&mut decoder, &[], &mut output, &mut samples)?;
    decoder.finish()?;
    let info = decoder
        .stream_info()
        .ok_or("Wavey decoder did not receive STREAMINFO")?;
    if info.sample_rate != case.sample_rate
        || info.channels != u32::from(case.channels)
        || info.bits_per_sample != u32::from(case.bits_per_sample)
    {
        return Err(format!(
            "Wavey geometry differs for case {}: {}Hz/{}ch/{}bit",
            case.index, info.sample_rate, info.channels, info.bits_per_sample
        )
        .into());
    }
    Ok(samples)
}

fn pump_wavey(
    decoder: &mut Decoder,
    input: &[u8],
    output: &mut [i32],
    samples: &mut Vec<i32>,
) -> Result<(), AnyError> {
    let mut first = true;
    loop {
        let written = decoder.decode_i32(if first { input } else { &[] }, output)?;
        first = false;
        samples.extend_from_slice(&output[..written]);
        if written == 0 {
            return Ok(());
        }
    }
}

fn ffmpeg_encode(binary: &OsStr, input: &Path, output: &Path) -> Result<(), AnyError> {
    let result = Command::new(binary)
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(input)
        .args(["-map", "0:a:0", "-c:a", "flac", "-compression_level", "5"])
        .arg(output)
        .output()?;
    require_command(result, "FFmpeg encode")
}

fn libflac_encode(binary: &OsStr, input: &Path, output: &Path) -> Result<(), AnyError> {
    let result = Command::new(binary)
        .args(["--silent", "--force", "--output-name"])
        .arg(output)
        .arg(input)
        .output()?;
    require_command(result, "libFLAC encode")
}

fn ffmpeg_decode(
    binary: &OsStr,
    input: &Path,
    case: Case,
    directory: &Path,
) -> Result<Vec<i32>, AnyError> {
    let extension = if case.bits_per_sample == 16 {
        "s16le"
    } else {
        "s24le"
    };
    let codec = if case.bits_per_sample == 16 {
        "pcm_s16le"
    } else {
        "pcm_s24le"
    };
    let output = directory.join(format!("decode-{}-ffmpeg.{extension}", case.index));
    let result = Command::new(binary)
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(input)
        .args(["-map", "0:a:0", "-f", extension, "-acodec", codec])
        .arg(&output)
        .output()?;
    require_command(result, "FFmpeg decode")?;
    parse_raw_pcm(&fs::read(output)?, case.bits_per_sample)
}

fn libflac_decode(
    binary: &OsStr,
    input: &Path,
    case: Case,
    directory: &Path,
) -> Result<Vec<i32>, AnyError> {
    let output = directory.join(format!("decode-{}-libflac.raw", case.index));
    let result = Command::new(binary)
        .args([
            "--decode",
            "--silent",
            "--force",
            "--force-raw-format",
            "--endian=little",
            "--sign=signed",
            "--output-name",
        ])
        .arg(&output)
        .arg(input)
        .output()?;
    require_command(result, "libFLAC decode")?;
    parse_raw_pcm(&fs::read(output)?, case.bits_per_sample)
}

fn parse_raw_pcm(bytes: &[u8], bits_per_sample: u8) -> Result<Vec<i32>, AnyError> {
    let bytes_per_sample = usize::from(bits_per_sample / 8);
    if !bytes.len().is_multiple_of(bytes_per_sample) {
        return Err("reference decoder emitted a partial PCM sample".into());
    }
    let mut samples = Vec::with_capacity(bytes.len() / bytes_per_sample);
    if bits_per_sample == 16 {
        for sample in bytes.chunks_exact(2) {
            samples.push(i32::from(i16::from_le_bytes([sample[0], sample[1]])));
        }
    } else {
        for sample in bytes.chunks_exact(3) {
            let unsigned =
                u32::from(sample[0]) | (u32::from(sample[1]) << 8) | (u32::from(sample[2]) << 16);
            samples.push((unsigned << 8) as i32 >> 8);
        }
    }
    Ok(samples)
}

fn require_samples(
    case: Case,
    expected: &[i32],
    actual: &[i32],
    encoder: &str,
    decoder: &str,
) -> Result<(), AnyError> {
    if expected.len() != actual.len() {
        return Err(format!(
            "case {} {decoder} <- {encoder} sample count differs: expected {}, got {}",
            case.index,
            expected.len(),
            actual.len()
        )
        .into());
    }
    if let Some((index, (&expected, &actual))) = expected
        .iter()
        .zip(actual)
        .enumerate()
        .find(|(_, (expected, actual))| expected != actual)
    {
        return Err(format!(
            "case {} {decoder} <- {encoder} differs at sample {index}: expected {expected}, got {actual}",
            case.index
        )
        .into());
    }
    Ok(())
}

fn ensure_tool(binary: &OsString) -> Result<(), AnyError> {
    let result = Command::new(binary).arg("-version").output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!("{} -version failed", Path::new(binary).display()).into())
    }
}

fn require_command(output: Output, context: &str) -> Result<(), AnyError> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{context} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )
    .into())
}
