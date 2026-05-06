use heic_decoder::{DecodedRgbaImage, DecodedRgbaPixels, decode_path_to_rgba, decode_read_to_rgba};
use std::env;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecodeMode {
    Path,
    Read,
}

#[derive(Debug)]
struct CliError(String);

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for CliError {}

fn usage() -> &'static str {
    "Usage: heif-stream-concurrency-bench <path|read> <workers> <iterations-per-worker> <input.heic|.avif> [more inputs...]"
}

fn parse_decode_mode(value: &str) -> Result<DecodeMode, CliError> {
    match value {
        "path" => Ok(DecodeMode::Path),
        "read" => Ok(DecodeMode::Read),
        other => Err(CliError(format!("Unsupported mode '{other}'. {}", usage()))),
    }
}

fn parse_positive_usize(value: &str, label: &str) -> Result<usize, CliError> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| CliError(format!("Invalid {label} '{value}'. {}", usage())))?;
    if parsed == 0 {
        return Err(CliError(format!(
            "{label} must be greater than zero. {}",
            usage()
        )));
    }
    Ok(parsed)
}

fn parse_args() -> Result<(DecodeMode, usize, usize, Vec<PathBuf>), CliError> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 5 {
        return Err(CliError(usage().to_string()));
    }

    let mode = parse_decode_mode(&args[1])?;
    let worker_count = parse_positive_usize(&args[2], "workers")?;
    let iterations = parse_positive_usize(&args[3], "iterations-per-worker")?;

    let mut input_paths = Vec::with_capacity(args.len() - 4);
    for raw in &args[4..] {
        let path = PathBuf::from(raw);
        if !path.is_file() {
            return Err(CliError(format!(
                "Input file not found: {}",
                path.display()
            )));
        }
        input_paths.push(path);
    }

    Ok((mode, worker_count, iterations, input_paths))
}

fn small_checksum(samples: &[u8]) -> u64 {
    if samples.is_empty() {
        return 0;
    }

    let first = samples[0] as u64;
    let middle = samples[samples.len() / 2] as u64;
    let last = samples[samples.len() - 1] as u64;
    (first << 16) ^ (middle << 8) ^ last ^ samples.len() as u64
}

fn small_checksum_u16(samples: &[u16]) -> u64 {
    if samples.is_empty() {
        return 0;
    }

    let first = samples[0] as u64;
    let middle = samples[samples.len() / 2] as u64;
    let last = samples[samples.len() - 1] as u64;
    (first << 32) ^ (middle << 16) ^ last ^ samples.len() as u64
}

fn decoded_image_checksum(image: &DecodedRgbaImage) -> u64 {
    let checksum = match &image.pixels {
        DecodedRgbaPixels::U8(samples) => small_checksum(samples),
        DecodedRgbaPixels::U16(samples) => small_checksum_u16(samples),
    };
    ((image.width as u64) << 32) ^ (image.height as u64) ^ checksum
}

fn decode_checksum_via_path(input_path: &Path) -> Result<u64, CliError> {
    let decoded = decode_path_to_rgba(input_path).map_err(|error| {
        CliError(format!(
            "path decode failed for {}: {error}",
            input_path.display()
        ))
    })?;
    Ok(decoded_image_checksum(&decoded))
}

fn decode_checksum(mode: DecodeMode, input_path: &Path) -> Result<u64, CliError> {
    match mode {
        DecodeMode::Path => decode_checksum_via_path(input_path),
        DecodeMode::Read => {
            let file = File::open(input_path).map_err(|error| {
                CliError(format!("failed to open {}: {error}", input_path.display()))
            })?;
            let decoded = decode_read_to_rgba(file).map_err(|error| {
                CliError(format!(
                    "read decode failed for {}: {error}",
                    input_path.display()
                ))
            })?;
            Ok(decoded_image_checksum(&decoded))
        }
    }
}

fn run_worker(
    worker_id: usize,
    mode: DecodeMode,
    iterations: usize,
    input_paths: Arc<Vec<PathBuf>>,
    expected_checksums: Arc<Vec<u64>>,
) -> Result<u64, CliError> {
    let mut aggregate_checksum = 0_u64;
    let input_count = input_paths.len();

    for iteration in 0..iterations {
        let input_index = (worker_id + iteration) % input_count;
        let input_path = &input_paths[input_index];
        let expected_checksum = expected_checksums[input_index];
        let actual_checksum = decode_checksum(mode, input_path)?;

        if actual_checksum != expected_checksum {
            return Err(CliError(format!(
                "checksum mismatch for {} in mode {:?}: expected={expected_checksum}, actual={actual_checksum}",
                input_path.display(),
                mode
            )));
        }

        let rotation = ((worker_id + iteration) % 63 + 1) as u32;
        aggregate_checksum ^= actual_checksum.rotate_left(rotation);
    }

    Ok(aggregate_checksum)
}

fn main() -> Result<(), Box<dyn Error>> {
    let (mode, worker_count, iterations, input_paths) = parse_args()?;

    let mut expected_checksums = Vec::with_capacity(input_paths.len());
    for input_path in &input_paths {
        expected_checksums.push(decode_checksum_via_path(input_path)?);
    }

    let input_paths = Arc::new(input_paths);
    let expected_checksums = Arc::new(expected_checksums);
    let mut handles = Vec::with_capacity(worker_count);

    for worker_id in 0..worker_count {
        let worker_inputs = Arc::clone(&input_paths);
        let worker_expected = Arc::clone(&expected_checksums);
        handles.push(thread::spawn(move || {
            run_worker(worker_id, mode, iterations, worker_inputs, worker_expected)
        }));
    }

    let mut aggregate_checksum = 0_u64;
    for handle in handles {
        let worker_checksum = handle
            .join()
            .map_err(|_| CliError("worker thread panicked".to_string()))??;
        aggregate_checksum ^= worker_checksum;
    }

    let total_ops = worker_count
        .checked_mul(iterations)
        .ok_or_else(|| CliError("total operation count overflow".to_string()))?;
    println!("ops={total_ops} checksum={aggregate_checksum}");

    Ok(())
}
