use heic_decoder::DecodeOptions;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage(program_name: &str) {
    eprintln!(
        "Usage: {program_name} [--orientation <auto|preserve>] [--max-input-bytes <bytes>] [--max-memory-bytes <bytes>] [--max-pixels <pixels>] [--max-temp-spool-bytes <bytes>] [--temp-spool-directory <path>] <input.heif|.heic|.avif> <output.png>"
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrientationMode {
    Auto,
    Preserve,
}

#[derive(Debug, Eq, PartialEq)]
struct CliInvocation {
    input_path: PathBuf,
    output_path: PathBuf,
    guardrails: DecodeOptions,
    orientation_mode: OrientationMode,
}

#[derive(Debug, Eq, PartialEq)]
enum CliParseError {
    HelpRequested,
    InvalidArguments(String),
}

fn parse_u64_option(flag: &str, value: String) -> Result<u64, CliParseError> {
    value.parse::<u64>().map_err(|_| {
        CliParseError::InvalidArguments(format!("{flag} expects a u64 value, got '{value}'"))
    })
}

fn parse_orientation_mode(value: String) -> Result<OrientationMode, CliParseError> {
    match value.as_str() {
        "auto" => Ok(OrientationMode::Auto),
        "preserve" => Ok(OrientationMode::Preserve),
        _ => Err(CliParseError::InvalidArguments(format!(
            "--orientation expects one of: auto, preserve (got '{value}')"
        ))),
    }
}

fn parse_cli_invocation<I>(args: I) -> Result<CliInvocation, CliParseError>
where
    I: IntoIterator<Item = String>,
{
    let mut positional = Vec::new();
    let mut guardrails = DecodeOptions::default();
    let mut orientation_mode = OrientationMode::Auto;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => return Err(CliParseError::HelpRequested),
            "--orientation" => {
                let value = iter.next().ok_or_else(|| {
                    CliParseError::InvalidArguments("missing value for --orientation".to_string())
                })?;
                orientation_mode = parse_orientation_mode(value)?;
            }
            "--max-input-bytes" => {
                let value = iter.next().ok_or_else(|| {
                    CliParseError::InvalidArguments(
                        "missing value for --max-input-bytes".to_string(),
                    )
                })?;
                guardrails.max_input_bytes = Some(parse_u64_option("--max-input-bytes", value)?);
            }
            "--max-memory-bytes" => {
                let value = iter.next().ok_or_else(|| {
                    CliParseError::InvalidArguments(
                        "missing value for --max-memory-bytes".to_string(),
                    )
                })?;
                guardrails.max_memory_bytes = Some(parse_u64_option("--max-memory-bytes", value)?);
            }
            "--max-pixels" => {
                let value = iter.next().ok_or_else(|| {
                    CliParseError::InvalidArguments("missing value for --max-pixels".to_string())
                })?;
                guardrails.max_pixels = Some(parse_u64_option("--max-pixels", value)?);
            }
            "--max-temp-spool-bytes" => {
                let value = iter.next().ok_or_else(|| {
                    CliParseError::InvalidArguments(
                        "missing value for --max-temp-spool-bytes".to_string(),
                    )
                })?;
                guardrails.max_temp_spool_bytes =
                    Some(parse_u64_option("--max-temp-spool-bytes", value)?);
            }
            "--temp-spool-directory" => {
                let value = iter.next().ok_or_else(|| {
                    CliParseError::InvalidArguments(
                        "missing value for --temp-spool-directory".to_string(),
                    )
                })?;
                guardrails.temp_spool_directory = Some(PathBuf::from(value));
            }
            _ if arg.starts_with('-') => {
                return Err(CliParseError::InvalidArguments(format!(
                    "unknown option '{arg}'"
                )));
            }
            _ => positional.push(arg),
        }
    }

    if positional.len() != 2 {
        return Err(CliParseError::InvalidArguments(
            "expected <input> and <output> positional arguments".to_string(),
        ));
    }

    Ok(CliInvocation {
        input_path: PathBuf::from(&positional[0]),
        output_path: PathBuf::from(&positional[1]),
        guardrails,
        orientation_mode,
    })
}

fn format_decode_failure(err: &heic_decoder::DecodeError) -> String {
    format!(
        "Decode failed [category={}]: {err}",
        err.category().as_str()
    )
}

fn main() -> ExitCode {
    let mut args = std::env::args();
    let program_name = args.next().unwrap_or_else(|| "heif-decode".to_string());
    let invocation = match parse_cli_invocation(args) {
        Ok(invocation) => invocation,
        Err(CliParseError::HelpRequested) => {
            usage(&program_name);
            return ExitCode::SUCCESS;
        }
        Err(CliParseError::InvalidArguments(message)) => {
            eprintln!("{message}");
            usage(&program_name);
            return ExitCode::from(2);
        }
    };

    let input_path = Path::new(&invocation.input_path);
    let output_path = Path::new(&invocation.output_path);
    let mut decoded = match heic_decoder::decode_path_to_rgba_with_options(
        input_path,
        invocation.guardrails.clone(),
    ) {
        Ok(decoded) => decoded,
        Err(err) => {
            eprintln!("{}", format_decode_failure(&err));
            return ExitCode::from(1);
        }
    };

    if invocation.orientation_mode == OrientationMode::Auto
        && heic_decoder::path_extension_is_heif(input_path)
    {
        let mut metadata_options = invocation.guardrails.clone();
        if let Some(limit) = metadata_options.max_memory_bytes {
            let pixels = match &decoded.pixels {
                heic_decoder::DecodedRgbaPixels::U8(pixels) => pixels.capacity() as u64,
                heic_decoder::DecodedRgbaPixels::U16(pixels) => pixels.capacity() as u64 * 2,
            };
            let icc = decoded
                .icc_profile
                .as_ref()
                .map_or(0, |icc| icc.capacity() as u64);
            metadata_options.max_memory_bytes =
                Some(limit.saturating_sub(pixels.saturating_add(icc)));
        }
        match heic_decoder::exif_orientation_hint_from_path_with_options(
            input_path,
            metadata_options,
        ) {
            Ok(hint) => {
                if let Some(orientation) = hint.orientation_to_apply() {
                    match decoded.apply_exif_orientation(orientation) {
                        Ok(oriented) => decoded = oriented,
                        Err(err) => {
                            eprintln!("{}", format_decode_failure(&err));
                            return ExitCode::from(1);
                        }
                    }
                }
            }
            Err(err) => {
                if invocation.guardrails.max_memory_bytes.is_some() {
                    eprintln!("{}", format_decode_failure(&err));
                    return ExitCode::from(1);
                }
                eprintln!(
                    "warning: failed to inspect EXIF orientation for {}: {err}",
                    input_path.display()
                );
            }
        }
    }

    match heic_decoder::write_decoded_rgba_to_png_with_options(
        &decoded,
        output_path,
        invocation.guardrails.clone(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}", format_decode_failure(&err));
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CliInvocation, CliParseError, OrientationMode, format_decode_failure, parse_cli_invocation,
    };
    use heic_decoder::{DecodeError, DecodeHeicError, DecodeOptions};
    use std::path::PathBuf;

    #[test]
    fn formats_decode_failure_with_structured_category() {
        let err = DecodeError::Unsupported("unsupported extension".to_string());
        assert_eq!(
            format_decode_failure(&err),
            "Decode failed [category=unsupported-feature]: unsupported extension"
        );
    }

    #[test]
    fn formats_nested_decode_failure_with_malformed_category() {
        let err = DecodeError::HeicDecode(DecodeHeicError::MissingSpsNalUnit);
        assert_eq!(
            format_decode_failure(&err),
            "Decode failed [category=malformed-input]: length-prefixed HEVC stream does not contain an SPS NAL unit"
        );
    }

    #[test]
    fn parses_cli_with_required_positional_arguments_only() {
        assert_eq!(
            parse_cli_invocation(["input.heic".to_string(), "output.png".to_string(),]),
            Ok(CliInvocation {
                input_path: PathBuf::from("input.heic"),
                output_path: PathBuf::from("output.png"),
                guardrails: DecodeOptions::default(),
                orientation_mode: OrientationMode::Auto,
            })
        );
    }

    #[test]
    fn parses_cli_with_all_guardrail_flags() {
        assert_eq!(
            parse_cli_invocation([
                "--max-input-bytes".to_string(),
                "1048576".to_string(),
                "--max-memory-bytes".to_string(),
                "268435456".to_string(),
                "--orientation".to_string(),
                "preserve".to_string(),
                "--max-pixels".to_string(),
                "262144".to_string(),
                "--max-temp-spool-bytes".to_string(),
                "2097152".to_string(),
                "--temp-spool-directory".to_string(),
                "/tmp/heic-decoder-spool".to_string(),
                "input.heic".to_string(),
                "output.png".to_string(),
            ]),
            Ok(CliInvocation {
                input_path: PathBuf::from("input.heic"),
                output_path: PathBuf::from("output.png"),
                guardrails: DecodeOptions {
                    max_input_bytes: Some(1_048_576),
                    max_memory_bytes: Some(268_435_456),
                    max_pixels: Some(262_144),
                    max_temp_spool_bytes: Some(2_097_152),
                    temp_spool_directory: Some(PathBuf::from("/tmp/heic-decoder-spool")),
                },
                orientation_mode: OrientationMode::Preserve,
            })
        );
    }

    #[test]
    fn rejects_invalid_orientation_mode() {
        assert_eq!(
            parse_cli_invocation([
                "--orientation".to_string(),
                "invalid".to_string(),
                "input.heic".to_string(),
                "output.png".to_string(),
            ]),
            Err(CliParseError::InvalidArguments(
                "--orientation expects one of: auto, preserve (got 'invalid')".to_string()
            ))
        );
    }

    #[test]
    fn rejects_unknown_cli_options() {
        assert_eq!(
            parse_cli_invocation([
                "--unknown".to_string(),
                "input.heic".to_string(),
                "output.png".to_string(),
            ]),
            Err(CliParseError::InvalidArguments(
                "unknown option '--unknown'".to_string()
            ))
        );
    }

    #[test]
    fn rejects_non_numeric_guardrail_values() {
        assert_eq!(
            parse_cli_invocation([
                "--max-input-bytes".to_string(),
                "NaN".to_string(),
                "input.heic".to_string(),
                "output.png".to_string(),
            ]),
            Err(CliParseError::InvalidArguments(
                "--max-input-bytes expects a u64 value, got 'NaN'".to_string()
            ))
        );
    }

    #[test]
    fn accepts_help_flag() {
        assert_eq!(
            parse_cli_invocation(["--help".to_string()]),
            Err(CliParseError::HelpRequested)
        );
    }
}
