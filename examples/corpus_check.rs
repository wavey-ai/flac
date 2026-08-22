//! Differential and robustness checks for external FLAC corpora.
//!
//! Usage:
//!
//! ```text
//! cargo run --release --example corpus_check -- \
//!   --required /path/to/valid/files \
//!   --optional /path/to/uncommon/files \
//!   --robustness /path/to/faulty-or-fuzz/files
//! ```

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;

use walkdir::WalkDir;
use wavey_flac::decoder::metadata::StreamInfo;
use wavey_flac::stream::Decoder;

const OUTPUT_SAMPLES: usize = 64 * 1024;
const CHUNK_SIZES: [usize; 7] = [1, 7, 251, 4_093, 65_536, 1_048_576, 16_381];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expectation {
    Required,
    Optional,
    Robustness,
}

#[derive(Debug)]
struct CorpusRoot {
    expectation: Expectation,
    path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct Decoded {
    info: StreamInfo,
    samples: Vec<i32>,
}

#[derive(Debug, PartialEq, Eq)]
struct ReferenceDecoded {
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    samples: Vec<i32>,
}

#[derive(Default)]
struct Summary {
    compared: usize,
    rejected_optional: usize,
    robustness_accepted: usize,
    robustness_rejected: usize,
    failures: Vec<String>,
}

fn usage() -> &'static str {
    "usage: corpus_check [--required|--optional|--robustness] PATH ..."
}

fn parse_args() -> Result<Vec<CorpusRoot>, String> {
    let mut expectation = None;
    let mut roots = Vec::new();
    for argument in env::args_os().skip(1) {
        match argument.to_str() {
            Some("--required") => expectation = Some(Expectation::Required),
            Some("--optional") => expectation = Some(Expectation::Optional),
            Some("--robustness") => expectation = Some(Expectation::Robustness),
            Some(value) if value.starts_with('-') => {
                return Err(format!("unknown option {value}\n{}", usage()))
            }
            _ => {
                let expectation = expectation.ok_or_else(|| usage().to_owned())?;
                roots.push(CorpusRoot {
                    expectation,
                    path: argument.into(),
                });
            }
        }
    }
    if roots.is_empty() {
        return Err(usage().to_owned());
    }
    Ok(roots)
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    if root.is_file() {
        return match root.extension().and_then(OsStr::to_str) {
            Some(extension) if extension.eq_ignore_ascii_case("flac") => Ok(vec![root.to_owned()]),
            _ => Err(format!("{} is not a FLAC file", root.display())),
        };
    }
    if !root.is_dir() {
        return Err(format!("corpus root {} does not exist", root.display()));
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| format!("walk {}: {error}", root.display()))?;
        if entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("flac"))
        {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

fn pump(decoder: &mut Decoder, input: &[u8], samples: &mut Vec<i32>) -> Result<(), String> {
    let mut output = vec![0_i32; OUTPUT_SAMPLES];
    let mut first = true;
    loop {
        let written = decoder
            .decode_i32(if first { input } else { &[] }, &mut output)
            .map_err(|error| error.to_string())?;
        first = false;
        samples.extend_from_slice(&output[..written]);
        if written == 0 {
            return Ok(());
        }
    }
}

fn decode_wavey(path: &Path) -> Result<Decoded, String> {
    let input = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut decoder = Decoder::new();
    let mut samples = Vec::new();
    let mut offset = 0;
    let mut chunk_index = 0;
    while offset < input.len() {
        let chunk_size = CHUNK_SIZES[chunk_index % CHUNK_SIZES.len()];
        let end = input.len().min(offset + chunk_size);
        pump(&mut decoder, &input[offset..end], &mut samples)?;
        offset = end;
        chunk_index += 1;
    }
    pump(&mut decoder, &[], &mut samples)?;
    decoder.finish().map_err(|error| error.to_string())?;
    let info = decoder
        .stream_info()
        .ok_or_else(|| "decoder produced no STREAMINFO".to_owned())?;
    Ok(Decoded { info, samples })
}

fn read_le_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| "truncated WAVE integer".to_owned())?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_le_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated WAVE integer".to_owned())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn parse_pcm_wave(bytes: &[u8]) -> Result<ReferenceDecoded, String> {
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("libFLAC output is not a RIFF/WAVE file".to_owned());
    }

    let mut format = None;
    let mut data = None;
    let mut offset = 12usize;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let id = &bytes[offset..offset + 4];
        let declared_len = read_le_u32(bytes, offset + 4)? as usize;
        let payload = offset + 8;
        let end = payload
            .checked_add(declared_len)
            .ok_or_else(|| "WAVE chunk length overflow".to_owned())?;
        if end > bytes.len() {
            return Err("truncated WAVE chunk".to_owned());
        }
        match id {
            b"fmt " => format = Some(&bytes[payload..end]),
            b"data" => data = Some(&bytes[payload..end]),
            _ => {}
        }
        offset = end
            .checked_add(declared_len & 1)
            .ok_or_else(|| "WAVE chunk padding overflow".to_owned())?;
    }

    let format = format.ok_or_else(|| "WAVE has no fmt chunk".to_owned())?;
    if format.len() < 16 {
        return Err("truncated WAVE fmt chunk".to_owned());
    }
    let format_tag = read_le_u16(format, 0)?;
    let channels = read_le_u16(format, 2)?;
    let sample_rate = read_le_u32(format, 4)?;
    let block_align = read_le_u16(format, 12)?;
    let container_bits = read_le_u16(format, 14)?;
    let bits_per_sample = match format_tag {
        1 => container_bits,
        0xfffe => {
            if format.len() < 40 || read_le_u16(format, 16)? < 22 {
                return Err("truncated WAVE_FORMAT_EXTENSIBLE fmt chunk".to_owned());
            }
            if read_le_u16(format, 24)? != 1 {
                return Err("libFLAC emitted non-PCM WAVE data".to_owned());
            }
            read_le_u16(format, 18)?
        }
        other => return Err(format!("unsupported WAVE format tag {other:#06x}")),
    };
    if channels == 0 || !matches!(container_bits, 8 | 16 | 24 | 32) {
        return Err(format!(
            "unsupported WAVE geometry: {channels} channels, {container_bits} container bits"
        ));
    }
    if bits_per_sample == 0 || bits_per_sample > container_bits {
        return Err(format!(
            "invalid WAVE valid-bit count {bits_per_sample} for {container_bits}-bit container"
        ));
    }

    let bytes_per_sample = usize::from(container_bits / 8);
    let expected_align = usize::from(channels) * bytes_per_sample;
    if usize::from(block_align) != expected_align {
        return Err(format!(
            "invalid WAVE block alignment {block_align}, expected {expected_align}"
        ));
    }
    let data = data.ok_or_else(|| "WAVE has no data chunk".to_owned())?;
    if data.len() % bytes_per_sample != 0 {
        return Err("WAVE data ends in a partial sample".to_owned());
    }

    let unused_bits = u32::from(container_bits - bits_per_sample);
    let mut samples = Vec::with_capacity(data.len() / bytes_per_sample);
    for sample in data.chunks_exact(bytes_per_sample) {
        let value = match container_bits {
            8 => i32::from(sample[0]) - 128,
            16 => i32::from(i16::from_le_bytes([sample[0], sample[1]])),
            24 => {
                let unsigned = u32::from(sample[0])
                    | (u32::from(sample[1]) << 8)
                    | (u32::from(sample[2]) << 16);
                (unsigned << 8) as i32 >> 8
            }
            32 => i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
            _ => unreachable!(),
        };
        samples.push(value >> unused_bits);
    }

    Ok(ReferenceDecoded {
        sample_rate,
        channels,
        bits_per_sample,
        samples,
    })
}

fn decode_libflac(path: &Path) -> Result<ReferenceDecoded, String> {
    let binary = env::var_os("FLAC").unwrap_or_else(|| "flac".into());
    let directory = tempfile::tempdir()
        .map_err(|error| format!("create temporary directory for libFLAC: {error}"))?;
    let output_path = directory.path().join("decoded.wav");
    let output = Command::new(binary)
        .args(["--decode", "--silent", "--force", "--output-name"])
        .arg(&output_path)
        .arg(path)
        .output()
        .map_err(|error| format!("launch libFLAC for {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "libFLAC rejected {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let bytes = fs::read(&output_path)
        .map_err(|error| format!("read libFLAC WAV for {}: {error}", path.display()))?;
    parse_pcm_wave(&bytes)
        .map_err(|error| format!("parse libFLAC WAV for {}: {error}", path.display()))
}

fn decode_libsndfile(path: &Path) -> Result<ReferenceDecoded, String> {
    let binary = env::var_os("SNDFILE_CONVERT").unwrap_or_else(|| "sndfile-convert".into());
    let directory = tempfile::tempdir()
        .map_err(|error| format!("create temporary directory for libsndfile: {error}"))?;
    let output_path = directory.path().join("decoded.wav");
    let output = Command::new(binary)
        .arg(path)
        .arg(&output_path)
        .output()
        .map_err(|error| format!("launch libsndfile for {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "libsndfile rejected {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let bytes = fs::read(&output_path)
        .map_err(|error| format!("read libsndfile WAV for {}: {error}", path.display()))?;
    parse_pcm_wave(&bytes)
        .map_err(|error| format!("parse libsndfile WAV for {}: {error}", path.display()))
}

fn decode_reference(path: &Path) -> Result<(&'static str, ReferenceDecoded), String> {
    match decode_libflac(path) {
        Ok(decoded) => Ok(("libFLAC", decoded)),
        Err(libflac_error) => match decode_libsndfile(path) {
            Ok(decoded) => Ok(("libsndfile", decoded)),
            Err(libsndfile_error) => Err(format!("{libflac_error}; {libsndfile_error}")),
        },
    }
}

fn decode_ffmpeg(path: &Path, bits_per_sample: u32) -> Result<Vec<i32>, String> {
    let binary = env::var_os("FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let directory = tempfile::tempdir()
        .map_err(|error| format!("create temporary directory for FFmpeg: {error}"))?;
    let output_path = directory.path().join("decoded.raw");
    let (format, codec, container_bits) = if bits_per_sample <= 16 {
        ("s16le", "pcm_s16le", 16)
    } else if bits_per_sample <= 32 {
        ("s32le", "pcm_s32le", 32)
    } else {
        return Err(format!("unsupported FFmpeg PCM depth {bits_per_sample}"));
    };
    let output = Command::new(binary)
        .args(["-v", "error", "-nostdin", "-y", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", format, "-acodec", codec])
        .arg(&output_path)
        .output()
        .map_err(|error| format!("launch FFmpeg for {}: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "FFmpeg rejected {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let bytes = fs::read(&output_path)
        .map_err(|error| format!("read FFmpeg PCM for {}: {error}", path.display()))?;
    let bytes_per_sample = container_bits / 8;
    if bytes.len() % bytes_per_sample != 0 {
        return Err(format!(
            "FFmpeg PCM for {} ends in a partial sample",
            path.display()
        ));
    }
    let shift = container_bits as u32 - bits_per_sample;
    let mut samples = Vec::with_capacity(bytes.len() / bytes_per_sample);
    if container_bits == 16 {
        for sample in bytes.chunks_exact(2) {
            samples.push(i32::from(i16::from_le_bytes([sample[0], sample[1]])) >> shift);
        }
    } else {
        for sample in bytes.chunks_exact(4) {
            samples.push(i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) >> shift);
        }
    }
    Ok(samples)
}

fn compare_samples(
    path: &Path,
    wavey: &[i32],
    reference: &[i32],
    oracle: &str,
) -> Result<(), String> {
    if wavey.len() != reference.len() {
        return Err(format!(
            "{} sample count differs: Wavey={}, {oracle}={}",
            path.display(),
            wavey.len(),
            reference.len()
        ));
    }
    if let Some((index, (&wavey, &reference))) = wavey
        .iter()
        .zip(reference)
        .enumerate()
        .find(|(_, (wavey, reference))| wavey != reference)
    {
        return Err(format!(
            "{} differs at interleaved sample {index}: Wavey={wavey}, {oracle}={reference}",
            path.display()
        ));
    }
    Ok(())
}

fn compare_reference(path: &Path, decoded: &Decoded) -> Result<(), String> {
    let (oracle, reference) = decode_reference(path)?;
    if decoded.info.sample_rate != reference.sample_rate
        || decoded.info.channels != u32::from(reference.channels)
        || decoded.info.bits_per_sample != u32::from(reference.bits_per_sample)
    {
        return Err(format!(
            "{} geometry differs: Wavey={}Hz/{}ch/{}bit, {oracle}={}Hz/{}ch/{}bit",
            path.display(),
            decoded.info.sample_rate,
            decoded.info.channels,
            decoded.info.bits_per_sample,
            reference.sample_rate,
            reference.channels,
            reference.bits_per_sample
        ));
    }
    compare_samples(path, &decoded.samples, &reference.samples, oracle)?;

    let ffmpeg = decode_ffmpeg(path, decoded.info.bits_per_sample)?;
    compare_samples(path, &decoded.samples, &ffmpeg, "FFmpeg")
}

fn check_file(path: &Path, expectation: Expectation, summary: &mut Summary) {
    let decoded = panic::catch_unwind(AssertUnwindSafe(|| decode_wavey(path)));
    match (expectation, decoded) {
        (_, Err(payload)) => {
            let detail = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("non-string panic");
            summary
                .failures
                .push(format!("PANIC {}: {detail}", path.display()));
        }
        (Expectation::Required, Ok(Err(error))) => summary
            .failures
            .push(format!("REJECT {}: {error}", path.display())),
        (Expectation::Required, Ok(Ok(decoded))) => match compare_reference(path, &decoded) {
            Ok(()) => {
                summary.compared += 1;
                println!("PASS {}", path.display());
            }
            Err(error) => summary.failures.push(error),
        },
        (Expectation::Optional, Ok(Err(error))) => {
            summary.rejected_optional += 1;
            println!("REJECT-SAFE {}: {error}", path.display());
        }
        (Expectation::Optional, Ok(Ok(decoded))) => match compare_reference(path, &decoded) {
            Ok(()) => {
                summary.compared += 1;
                println!("PASS {}", path.display());
            }
            Err(error) => summary.failures.push(error),
        },
        (Expectation::Robustness, Ok(Err(error))) => {
            summary.robustness_rejected += 1;
            println!("REJECT-SAFE {}: {error}", path.display());
        }
        (Expectation::Robustness, Ok(Ok(_))) => {
            summary.robustness_accepted += 1;
            println!("ACCEPT-SAFE {}", path.display());
        }
    }
}

fn run() -> Result<(), String> {
    let roots = parse_args()?;
    let mut summary = Summary::default();
    let mut files_seen = 0;
    for root in roots {
        let files = collect_files(&root.path)?;
        if files.is_empty() {
            return Err(format!("{} contains no FLAC files", root.path.display()));
        }
        for path in files {
            files_seen += 1;
            check_file(&path, root.expectation, &mut summary);
        }
    }

    println!(
        "SUMMARY files={files_seen} compared={} optional_rejections={} robustness_accepted={} robustness_rejected={} failures={}",
        summary.compared,
        summary.rejected_optional,
        summary.robustness_accepted,
        summary.robustness_rejected,
        summary.failures.len()
    );
    if summary.failures.is_empty() {
        return Ok(());
    }
    for failure in &summary.failures {
        eprintln!("FAIL {failure}");
    }
    Err(format!("{} corpus checks failed", summary.failures.len()))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
