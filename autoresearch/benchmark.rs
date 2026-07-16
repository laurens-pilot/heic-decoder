use heic_decoder::image_integration::register_image_decoder_hooks_with_guardrails;
use heic_decoder::DecodeGuardrails;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use std::env;
use std::error::Error;
use std::hint::black_box;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const BENCHMARK_VERSION: u32 = 2;

struct Options {
    warmup_rounds: usize,
    sample_rounds: usize,
    probe_compatible: bool,
    paths: Vec<PathBuf>,
}

struct Input {
    path: PathBuf,
    pixels: u64,
    fingerprint: u64,
}

struct HookOutput {
    image: DynamicImage,
    icc_profile: Option<Vec<u8>>,
}

fn usage() -> &'static str {
    "Usage: heic-autoresearch-bench [--warmup N] [--samples N] [--probe-compatible] <input.heic> [...]"
}

fn parse_count(flag: &str, value: Option<String>) -> Result<usize, String> {
    let value = value.ok_or_else(|| format!("missing value for {flag}"))?;
    value
        .parse::<usize>()
        .map_err(|_| format!("{flag} expects a non-negative integer, got '{value}'"))
}

fn parse_options() -> Result<Options, String> {
    let mut args = env::args().skip(1);
    let mut warmup_rounds = 1;
    let mut sample_rounds = 5;
    let mut probe_compatible = false;
    let mut paths = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--warmup" => warmup_rounds = parse_count("--warmup", args.next())?,
            "--samples" => sample_rounds = parse_count("--samples", args.next())?,
            "--probe-compatible" => probe_compatible = true,
            "--help" | "-h" => return Err(usage().to_string()),
            _ if arg.starts_with('-') => {
                return Err(format!("unknown option '{arg}'. {}", usage()));
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }

    if sample_rounds == 0 {
        return Err("--samples must be at least 1".to_string());
    }
    if paths.is_empty() {
        return Err(format!("at least one input is required. {}", usage()));
    }

    Ok(Options {
        warmup_rounds,
        sample_rounds,
        probe_compatible,
        paths,
    })
}

fn register_production_hooks() -> Result<(), Box<dyn Error>> {
    let registration = register_image_decoder_hooks_with_guardrails(DecodeGuardrails {
        max_input_bytes: Some(128 * 1024 * 1024),
        max_pixels: Some(256_000_000),
        max_temp_spool_bytes: Some(256 * 1024 * 1024),
        temp_spool_directory: None,
    });
    if !registration.heic_decoder_hook_registered || !registration.heif_decoder_hook_registered {
        return Err("HEIC/HEIF image-crate hooks were not registered".into());
    }
    Ok(())
}

/// Mirrors Ente's path-based image-crate hook call shape in
/// `rust/crates/image/src/decode.rs::decode_reader_with_image_crate`.
fn decode_through_image_hook(path: &Path) -> image::ImageResult<HookOutput> {
    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let icc_profile = decoder.icc_profile()?;

    let mut limits = Limits::default();
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;

    Ok(HookOutput {
        image: DynamicImage::from_decoder(decoder)?,
        icc_profile,
    })
}

fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn output_pixels_and_fingerprint(output: &HookOutput) -> Result<(u64, u64), Box<dyn Error>> {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    hash = fnv1a(&output.image.width().to_le_bytes(), hash);
    hash = fnv1a(&output.image.height().to_le_bytes(), hash);
    hash = match &output.image {
        DynamicImage::ImageRgba8(buffer) => fnv1a(buffer.as_raw(), hash),
        DynamicImage::ImageRgba16(buffer) => {
            for sample in buffer.as_raw() {
                hash = fnv1a(&sample.to_le_bytes(), hash);
            }
            hash
        }
        other => return Err(format!("image hook returned unsupported {:?}", other.color()).into()),
    };
    match &output.icc_profile {
        Some(profile) => {
            hash = fnv1a(&[1], hash);
            hash = fnv1a(profile, hash);
        }
        None => hash = fnv1a(&[0], hash),
    }
    Ok((
        u64::from(output.image.width()) * u64::from(output.image.height()),
        hash,
    ))
}

fn timed_decode(input: &Input) -> Result<Duration, Box<dyn Error>> {
    let started = Instant::now();
    let output = decode_through_image_hook(black_box(&input.path))?;
    black_box(output.image.width());
    black_box(output.image.height());
    black_box(output.icc_profile.as_ref().map(Vec::len));
    match &output.image {
        DynamicImage::ImageRgba8(buffer) => {
            black_box(buffer.as_raw().as_ptr());
            black_box(buffer.as_raw().len());
        }
        DynamicImage::ImageRgba16(buffer) => {
            black_box(buffer.as_raw().as_ptr());
            black_box(buffer.as_raw().len());
        }
        other => return Err(format!("image hook returned unsupported {:?}", other.color()).into()),
    }
    drop(output);
    Ok(started.elapsed())
}

fn round_order(input_count: usize, round: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..input_count).collect();
    order.rotate_left(round % input_count);
    if round % 2 == 1 {
        order.reverse();
    }
    order
}

fn median(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    let middle = samples.len() / 2;
    if samples.len() % 2 == 1 {
        samples[middle]
    } else {
        (samples[middle - 1] + samples[middle]) / 2
    }
}

fn probe_compatible(paths: &[PathBuf]) -> Result<(), Box<dyn Error>> {
    let mut compatible = 0;
    for path in paths {
        match decode_through_image_hook(path) {
            Ok(output) => {
                output_pixels_and_fingerprint(&output)?;
                println!("compatible: {}", path.display());
                compatible += 1;
            }
            Err(error) => eprintln!("incompatible: {}: {error}", path.display()),
        }
    }
    if compatible == 0 {
        return Err("no compatible image-crate hook inputs found".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = parse_options().map_err(|message| {
        eprintln!("{message}");
        io::Error::new(io::ErrorKind::InvalidInput, message)
    })?;
    register_production_hooks()?;
    if options.probe_compatible {
        return probe_compatible(&options.paths);
    }

    // The one-time correctness fingerprint and process startup are outside the
    // timed region. The score itself mirrors Ente's path-based ImageReader hook
    // flow, including file open/read, layout probing, ICC retrieval, limits,
    // caller-buffer decode, output allocation, and deallocation.
    let mut inputs = Vec::with_capacity(options.paths.len());
    let mut suite_pixels = 0_u64;
    let mut suite_fingerprint = 0_u64;
    for path in options.paths {
        let output = decode_through_image_hook(&path)?;
        let (pixels, output_fingerprint) = output_pixels_and_fingerprint(&output)?;
        suite_pixels = suite_pixels.saturating_add(pixels);
        suite_fingerprint ^= output_fingerprint.rotate_left((inputs.len() % 63 + 1) as u32);
        inputs.push(Input {
            path,
            pixels,
            fingerprint: output_fingerprint,
        });
    }

    for round in 0..options.warmup_rounds {
        for index in round_order(inputs.len(), round) {
            let duration = timed_decode(&inputs[index])?;
            black_box(duration);
        }
    }

    let mut totals = Vec::with_capacity(options.sample_rounds);
    let mut per_input = vec![Vec::with_capacity(options.sample_rounds); inputs.len()];
    for round in 0..options.sample_rounds {
        let mut total = Duration::ZERO;
        for index in round_order(inputs.len(), round + options.warmup_rounds) {
            let elapsed = timed_decode(&inputs[index])?;
            total += elapsed;
            per_input[index].push(elapsed);
        }
        totals.push(total);
    }

    let median_total = median(&mut totals);
    let score_ms = median_total.as_secs_f64() * 1_000.0;
    let throughput = suite_pixels as f64 / 1_000_000.0 / median_total.as_secs_f64();

    println!("benchmark_version: {BENCHMARK_VERSION}");
    println!("benchmark_path: image_crate_hook_path");
    println!("score_ms: {score_ms:.6}");
    println!("throughput_mpix_s: {throughput:.6}");
    println!("suite_pixels: {suite_pixels}");
    println!("suite_fingerprint: {suite_fingerprint:016x}");
    println!("files: {}", inputs.len());
    println!("samples: {}", options.sample_rounds);
    println!("warmup: {}", options.warmup_rounds);
    for (input, durations) in inputs.iter().zip(&mut per_input) {
        let input_median_ms = median(durations).as_secs_f64() * 1_000.0;
        println!(
            "file: path={} pixels={} median_ms={input_median_ms:.6} fingerprint={:016x}",
            input.path.display(),
            input.pixels,
            input.fingerprint
        );
    }

    Ok(())
}
