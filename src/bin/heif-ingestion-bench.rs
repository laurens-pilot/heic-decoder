use heic_decoder::{DecodedRgbaPixels, decode_bytes_to_rgba, decode_path_to_rgba};
use std::env;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DecodeMode {
    Path,
    Bytes,
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
    "Usage: heif-ingestion-bench <path|bytes> <input.heic|.heif|.avif>"
}

fn parse_args() -> Result<(DecodeMode, String), CliError> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        return Err(CliError(usage().to_string()));
    }

    let mode = match args[1].as_str() {
        "path" => DecodeMode::Path,
        "bytes" => DecodeMode::Bytes,
        other => return Err(CliError(format!("Unsupported mode '{other}'. {}", usage()))),
    };

    Ok((mode, args[2].clone()))
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

fn bench_path(input_path: &Path) -> Result<u64, Box<dyn Error>> {
    let decoded = decode_path_to_rgba(input_path)?;
    let checksum = match &decoded.pixels {
        DecodedRgbaPixels::U8(samples) => small_checksum(samples),
        DecodedRgbaPixels::U16(samples) => small_checksum_u16(samples),
    };

    Ok(((decoded.width as u64) << 32) ^ (decoded.height as u64) ^ checksum)
}

fn bench_bytes(input_path: &Path) -> Result<u64, Box<dyn Error>> {
    let input = fs::read(input_path)?;
    let decoded = decode_bytes_to_rgba(&input)?;
    let checksum = match &decoded.pixels {
        DecodedRgbaPixels::U8(samples) => small_checksum(samples),
        DecodedRgbaPixels::U16(samples) => small_checksum_u16(samples),
    };

    Ok(((decoded.width as u64) << 32) ^ (decoded.height as u64) ^ checksum)
}

fn main() -> Result<(), Box<dyn Error>> {
    let (mode, input_path) = parse_args()?;
    let input_path = Path::new(&input_path);

    let checksum = match mode {
        DecodeMode::Path => bench_path(input_path)?,
        DecodeMode::Bytes => bench_bytes(input_path)?,
    };

    println!("{checksum}");
    Ok(())
}
