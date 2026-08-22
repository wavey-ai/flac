//! Whole-file FLAC benchmark and losslessness gate against FFmpeg.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --example ffmpeg_baseline -- input.wav [output-dir] [runs]
//! ```

#![allow(clippy::manual_is_multiple_of)] // Keep the crate's Rust 1.65 MSRV.

use hound::{SampleFormat, WavReader};
use md5::{Digest, Md5};
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use wavey_flac::frame::{FlacFrameConfig, FlacProfile};
use wavey_flac::stream::{Decoder, Encoder};

const BLOCK_SIZE: u32 = 4_096;
const INPUT_CHUNK_BYTES: usize = 1024 * 1024;
const OUTPUT_CHUNK_SAMPLES: usize = 256 * 1024;

type AnyError = Box<dyn Error>;

#[derive(Clone, Copy)]
struct SourceInfo {
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    frames: u64,
}

impl SourceInfo {
    fn samples(self) -> u64 {
        self.frames * u64::from(self.channels)
    }

    fn pcm_bytes(self) -> u64 {
        self.samples() * u64::from(self.bits_per_sample / 8)
    }

    fn seconds(self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate)
    }
}

struct Timing {
    samples: Vec<Duration>,
}

impl Timing {
    fn median_seconds(&self) -> f64 {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let middle = sorted.len() / 2;
        if sorted.len() % 2 == 0 {
            (sorted[middle - 1].as_secs_f64() + sorted[middle].as_secs_f64()) / 2.0
        } else {
            sorted[middle].as_secs_f64()
        }
    }

    fn minimum_seconds(&self) -> f64 {
        self.samples
            .iter()
            .min()
            .expect("at least one benchmark run")
            .as_secs_f64()
    }
}

struct EncodedAsset {
    label: &'static str,
    path: PathBuf,
    timing: Timing,
    bytes: u64,
}

struct DecodeMeasurement {
    decoder: &'static str,
    input: &'static str,
    timing: Timing,
}

fn main() -> Result<(), AnyError> {
    let mut arguments = env::args_os().skip(1);
    let input = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: ffmpeg_baseline INPUT.wav [OUTPUT_DIR] [RUNS]")?;
    let output_dir = arguments
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/ffmpeg-baseline"));
    let runs = arguments
        .next()
        .map(|value| value.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(5);
    if arguments.next().is_some() || runs == 0 {
        return Err("RUNS must be a positive integer and the final argument".into());
    }

    let ffmpeg = env::var_os("FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    ensure_ffmpeg(&ffmpeg)?;
    fs::create_dir_all(&output_dir)?;

    let source = inspect_source(&input)?;
    if source.bits_per_sample != 24 {
        return Err(format!(
            "this baseline currently requires 24-bit integer PCM, found {} bits",
            source.bits_per_sample
        )
        .into());
    }
    if source.frames % u64::from(BLOCK_SIZE) != 0 && source.frames % u64::from(BLOCK_SIZE) < 32 {
        return Err(format!(
            "the final {}-sample block is below FLAC's 32-sample minimum",
            source.frames % u64::from(BLOCK_SIZE)
        )
        .into());
    }

    eprintln!(
        "Preparing {:.3} s of {} Hz, {}-channel, {}-bit PCM...",
        source.seconds(),
        source.sample_rate,
        source.channels,
        source.bits_per_sample
    );
    let canonical_pcm = output_dir.join("canonical.s24le");
    ffmpeg_decode_s24(&ffmpeg, &input, &canonical_pcm)?;
    let wavey_source_pcm = output_dir.join("wavey-source.s24le");
    render_wave_source_s24(&input, &wavey_source_pcm)?;
    require_identical(
        &canonical_pcm,
        &wavey_source_pcm,
        "Wavey WAV input",
        "FFmpeg WAV input",
    )?;

    eprintln!("Benchmarking encoders ({runs} measured runs after one warm-up)...");
    let balanced_path = output_dir.join("wavey-balanced.flac");
    let balanced_timing = benchmark(runs, || {
        wavey_encode(&input, &balanced_path, FlacProfile::Balanced)
    })?;
    let maximum_path = output_dir.join("wavey-maximum.flac");
    let maximum_timing = benchmark(runs, || {
        wavey_encode(&input, &maximum_path, FlacProfile::Maximum)
    })?;
    let ffmpeg_path = output_dir.join("ffmpeg-level-5.flac");
    let ffmpeg_timing = benchmark(runs, || ffmpeg_encode(&ffmpeg, &input, &ffmpeg_path, 5))?;

    let assets = vec![
        EncodedAsset {
            label: "Wavey balanced",
            bytes: fs::metadata(&balanced_path)?.len(),
            path: balanced_path,
            timing: balanced_timing,
        },
        EncodedAsset {
            label: "Wavey maximum",
            bytes: fs::metadata(&maximum_path)?.len(),
            path: maximum_path,
            timing: maximum_timing,
        },
        EncodedAsset {
            label: "FFmpeg level 5",
            bytes: fs::metadata(&ffmpeg_path)?.len(),
            path: ffmpeg_path,
            timing: ffmpeg_timing,
        },
    ];

    eprintln!("Benchmarking both decoders on every encoded file...");
    let mut decode_measurements = Vec::with_capacity(assets.len() * 2);
    let mut quality_paths = Vec::with_capacity(assets.len() * 2 + 1);
    quality_paths.push(("Wavey WAV input".to_string(), wavey_source_pcm));
    for (asset_index, asset) in assets.iter().enumerate() {
        let stem = format!("asset-{asset_index}");
        let wavey_output = output_dir.join(format!("{stem}-wavey.s24le"));
        let wavey_timing = benchmark(runs, || wavey_decode(&asset.path, &wavey_output))?;
        require_identical(
            &canonical_pcm,
            &wavey_output,
            "canonical PCM",
            &format!("Wavey decoding {}", asset.label),
        )?;
        quality_paths.push((format!("Wavey <- {}", asset.label), wavey_output));
        decode_measurements.push(DecodeMeasurement {
            decoder: "Wavey",
            input: asset.label,
            timing: wavey_timing,
        });

        let ffmpeg_output = output_dir.join(format!("{stem}-ffmpeg.s24le"));
        let ffmpeg_decode_timing = benchmark(runs, || {
            ffmpeg_decode_s24(&ffmpeg, &asset.path, &ffmpeg_output)
        })?;
        require_identical(
            &canonical_pcm,
            &ffmpeg_output,
            "canonical PCM",
            &format!("FFmpeg decoding {}", asset.label),
        )?;
        quality_paths.push((format!("FFmpeg <- {}", asset.label), ffmpeg_output));
        decode_measurements.push(DecodeMeasurement {
            decoder: "FFmpeg",
            input: asset.label,
            timing: ffmpeg_decode_timing,
        });
    }

    let pcm_md5 = md5_file(&canonical_pcm)?;
    let source_md5 = md5_file(&input)?;
    print_report(
        &input,
        &output_dir,
        &ffmpeg,
        source,
        runs,
        &source_md5,
        &pcm_md5,
        &assets,
        &decode_measurements,
        &quality_paths,
    )?;
    Ok(())
}

fn inspect_source(path: &Path) -> Result<SourceInfo, AnyError> {
    let reader = WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_format != SampleFormat::Int {
        return Err("the source WAV must contain integer PCM".into());
    }
    Ok(SourceInfo {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        bits_per_sample: spec.bits_per_sample,
        frames: u64::from(reader.duration()),
    })
}

fn render_wave_source_s24(input: &Path, output: &Path) -> Result<(), AnyError> {
    let mut reader = WavReader::open(input)?;
    let mut writer = BufWriter::new(File::create(output)?);
    let mut packed = Vec::with_capacity(OUTPUT_CHUNK_SAMPLES * 3);
    let mut samples = Vec::with_capacity(OUTPUT_CHUNK_SAMPLES);
    for sample in reader.samples::<i32>() {
        samples.push(sample?);
        if samples.len() == OUTPUT_CHUNK_SAMPLES {
            write_s24(&mut writer, &samples, &mut packed)?;
            samples.clear();
        }
    }
    write_s24(&mut writer, &samples, &mut packed)?;
    writer.flush()?;
    Ok(())
}

fn wavey_encode(input: &Path, output: &Path, profile: FlacProfile) -> Result<(), AnyError> {
    let mut reader = WavReader::open(input)?;
    let spec = reader.spec();
    let config = FlacFrameConfig::new(
        spec.sample_rate,
        spec.channels,
        spec.bits_per_sample as u8,
        BLOCK_SIZE,
        profile,
    )?;
    let mut encoder = Encoder::new(config)?;
    let header_len = encoder.stream_header().len();
    let samples_per_block = config.sample_count()?;
    let mut block = Vec::with_capacity(samples_per_block);
    let mut packet = Vec::new();
    let mut writer = BufWriter::new(File::create(output)?);
    writer.write_all(b"fLaC")?;

    for sample in reader.samples::<i32>() {
        block.push(sample?);
        if block.len() == samples_per_block {
            encoder.encode_i32(&block, &mut packet)?;
            writer.write_all(&packet)?;
            packet.clear();
            block.clear();
        }
    }
    if !block.is_empty() {
        encoder.encode_i32(&block, &mut packet)?;
        writer.write_all(&packet)?;
    }

    let final_header = encoder.finish()?.to_vec();
    if final_header.len() != header_len {
        return Err("final FLAC metadata changed size and cannot be backpatched".into());
    }
    writer.flush()?;
    writer.seek(SeekFrom::Start(4))?;
    writer.write_all(&final_header)?;
    writer.flush()?;
    Ok(())
}

fn wavey_decode(input: &Path, output: &Path) -> Result<(), AnyError> {
    let mut reader = BufReader::new(File::open(input)?);
    let mut writer = BufWriter::new(File::create(output)?);
    let mut decoder = Decoder::new();
    let mut compressed = vec![0_u8; INPUT_CHUNK_BYTES];
    let mut samples = vec![0_i32; OUTPUT_CHUNK_SAMPLES];
    let mut packed = Vec::with_capacity(OUTPUT_CHUNK_SAMPLES * 3);

    loop {
        let read = reader.read(&mut compressed)?;
        if read == 0 {
            break;
        }
        let written = decoder.decode_i32(&compressed[..read], &mut samples)?;
        write_s24(&mut writer, &samples[..written], &mut packed)?;
        loop {
            let written = decoder.decode_i32(&[], &mut samples)?;
            if written == 0 {
                break;
            }
            write_s24(&mut writer, &samples[..written], &mut packed)?;
        }
    }
    loop {
        let written = decoder.decode_i32(&[], &mut samples)?;
        if written == 0 {
            break;
        }
        write_s24(&mut writer, &samples[..written], &mut packed)?;
    }
    decoder.finish()?;
    let stream_info = decoder
        .stream_info()
        .ok_or("Wavey decoder did not receive STREAMINFO")?;
    if stream_info.bits_per_sample != 24 {
        return Err(format!(
            "benchmark output requires 24-bit PCM, decoded {} bits",
            stream_info.bits_per_sample
        )
        .into());
    }
    writer.flush()?;
    Ok(())
}

fn write_s24<W: Write>(
    writer: &mut W,
    samples: &[i32],
    packed: &mut Vec<u8>,
) -> Result<(), AnyError> {
    packed.clear();
    packed.reserve(samples.len() * 3);
    for &sample in samples {
        if !(-8_388_608..=8_388_607).contains(&sample) {
            return Err(format!("sample {sample} is outside signed 24-bit PCM").into());
        }
        packed.push((sample & 0xff) as u8);
        packed.push(((sample >> 8) & 0xff) as u8);
        packed.push(((sample >> 16) & 0xff) as u8);
    }
    writer.write_all(packed)?;
    Ok(())
}

fn benchmark<F>(runs: usize, mut operation: F) -> Result<Timing, AnyError>
where
    F: FnMut() -> Result<(), AnyError>,
{
    operation()?;
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        operation()?;
        samples.push(started.elapsed());
    }
    Ok(Timing { samples })
}

fn ensure_ffmpeg(ffmpeg: &OsStr) -> Result<(), AnyError> {
    let result = Command::new(ffmpeg).arg("-version").output()?;
    if !result.status.success() {
        return Err("ffmpeg -version failed".into());
    }
    Ok(())
}

fn ffmpeg_encode(
    ffmpeg: &OsStr,
    input: &Path,
    output: &Path,
    compression_level: u8,
) -> Result<(), AnyError> {
    let result = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(input)
        .args(["-map", "0:a:0", "-map_metadata", "-1", "-c:a", "flac"])
        .args(["-compression_level", &compression_level.to_string()])
        .arg(output)
        .status()?;
    if !result.success() {
        return Err(format!("FFmpeg FLAC encode failed with {result}").into());
    }
    Ok(())
}

fn ffmpeg_decode_s24(ffmpeg: &OsStr, input: &Path, output: &Path) -> Result<(), AnyError> {
    let result = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(input)
        .args([
            "-map",
            "0:a:0",
            "-map_metadata",
            "-1",
            "-f",
            "s24le",
            "-c:a",
            "pcm_s24le",
        ])
        .arg(output)
        .status()?;
    if !result.success() {
        return Err(format!("FFmpeg S24 decode failed with {result}").into());
    }
    Ok(())
}

fn require_identical(
    expected: &Path,
    actual: &Path,
    expected_label: &str,
    actual_label: &str,
) -> Result<(), AnyError> {
    let expected_len = fs::metadata(expected)?.len();
    let actual_len = fs::metadata(actual)?.len();
    if expected_len != actual_len {
        return Err(format!(
            "quality gate failed: {actual_label} has {actual_len} PCM bytes; {expected_label} has {expected_len}"
        )
        .into());
    }

    let mut expected_reader = BufReader::new(File::open(expected)?);
    let mut actual_reader = BufReader::new(File::open(actual)?);
    let mut expected_buffer = vec![0_u8; INPUT_CHUNK_BYTES];
    let mut actual_buffer = vec![0_u8; INPUT_CHUNK_BYTES];
    let mut offset = 0_u64;
    loop {
        let expected_read = expected_reader.read(&mut expected_buffer)?;
        let actual_read = actual_reader.read(&mut actual_buffer)?;
        if expected_read == 0 && actual_read == 0 {
            return Ok(());
        }
        if expected_read != actual_read {
            return Err(format!(
                "quality gate failed: {expected_label} and {actual_label} yielded different read lengths"
            )
            .into());
        }
        if let Some(index) = expected_buffer[..expected_read]
            .iter()
            .zip(&actual_buffer[..actual_read])
            .position(|(left, right)| left != right)
        {
            return Err(format!(
                "quality gate failed: {actual_label} differs from {expected_label} at PCM byte {}",
                offset + index as u64
            )
            .into());
        }
        offset += expected_read as u64;
    }
}

fn md5_file(path: &Path) -> Result<String, AnyError> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Md5::new();
    let mut buffer = vec![0_u8; INPUT_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[allow(clippy::too_many_arguments)]
fn print_report(
    input: &Path,
    output_dir: &Path,
    ffmpeg: &OsStr,
    source: SourceInfo,
    runs: usize,
    source_md5: &str,
    pcm_md5: &str,
    assets: &[EncodedAsset],
    decodes: &[DecodeMeasurement],
    quality_paths: &[(String, PathBuf)],
) -> Result<(), AnyError> {
    let ffmpeg_version = Command::new(ffmpeg).arg("-version").output()?;
    let ffmpeg_version = String::from_utf8_lossy(&ffmpeg_version.stdout)
        .lines()
        .next()
        .unwrap_or("unknown FFmpeg version")
        .to_string();
    println!("# FLAC whole-file baseline\n");
    println!("- Source: `{}`", input.display());
    println!("- Source file MD5: `{source_md5}`");
    println!("- Canonical decoded PCM MD5: `{pcm_md5}`");
    println!(
        "- Geometry: {} Hz, {} channels, {} bits, {} frames ({:.6} s)",
        source.sample_rate,
        source.channels,
        source.bits_per_sample,
        source.frames,
        source.seconds()
    );
    println!("- Frame size: {BLOCK_SIZE} samples/channel");
    println!("- Timing: median of {runs} runs after one warm-up; file I/O included");
    println!("- FFmpeg: `{ffmpeg_version}`");
    println!("- Artifacts: `{}`\n", output_dir.display());

    println!("## Encode\n");
    println!("| Encoder | Median | Minimum | Speed | FLAC bytes | Bitrate | PCM size |");
    println!("|---|---:|---:|---:|---:|---:|---:|");
    for asset in assets {
        let median = asset.timing.median_seconds();
        println!(
            "| {} | {:.3} s | {:.3} s | {:.1}x realtime | {} | {:.1} kb/s | {:.2}% |",
            asset.label,
            median,
            asset.timing.minimum_seconds(),
            source.seconds() / median,
            asset.bytes,
            asset.bytes as f64 * 8.0 / source.seconds() / 1_000.0,
            asset.bytes as f64 / source.pcm_bytes() as f64 * 100.0,
        );
    }

    println!("\n## Decode\n");
    println!("| Decoder | FLAC input | Median | Minimum | Speed | Quality |");
    println!("|---|---|---:|---:|---:|---|");
    for decode in decodes {
        let median = decode.timing.median_seconds();
        println!(
            "| {} | {} | {:.3} s | {:.3} s | {:.1}x realtime | bit-exact |",
            decode.decoder,
            decode.input,
            median,
            decode.timing.minimum_seconds(),
            source.seconds() / median,
        );
    }

    println!("\n## Quality gate\n");
    println!(
        "All paths produced {} PCM bytes with MD5 `{pcm_md5}`.",
        source.pcm_bytes()
    );
    println!(
        "Maximum absolute sample error: **0**; mismatched samples: **0**; SNR: **infinite**.\n"
    );
    println!("| Path | Result |");
    println!("|---|---|");
    for (label, _) in quality_paths {
        println!("| {label} | bit-exact |");
    }
    Ok(())
}
