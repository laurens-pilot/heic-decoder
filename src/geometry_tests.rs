use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static LARGEST_ALLOCATION: Cell<Option<usize>> = const { Cell::new(None) };
    static FAIL_ALLOCATION_AT: Cell<Option<usize>> = const { Cell::new(None) };
}

struct TrackingAllocator;

thread_local! {
    static LIVE_BYTES: Cell<usize> = const { Cell::new(0) };
    static LIVE_PEAK: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if FAIL_ALLOCATION_AT.with(|limit| limit.get().is_some_and(|limit| layout.size() >= limit))
        {
            return std::ptr::null_mut();
        }
        LARGEST_ALLOCATION.with(|largest| {
            if let Some(previous) = largest.get() {
                largest.set(Some(previous.max(layout.size())));
            }
        });
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.with(|live| {
                let now = live.get() + layout.size();
                live.set(now);
                LIVE_PEAK.with(|peak| peak.set(peak.get().max(now)));
            });
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.with(|live| live.set(live.get().saturating_sub(layout.size())));
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[test]
fn hevc_frame_allocation_failure_retains_bounded_error_variant() {
    FAIL_ALLOCATION_AT.with(|limit| limit.set(Some(512)));
    let result = HeicFrame::try_with_params(16, 16, 8, 1);
    FAIL_ALLOCATION_AT.with(|limit| limit.set(None));
    assert!(matches!(
        BoundedDecodeError::from(result.unwrap_err()),
        BoundedDecodeError::AllocationFailed
    ));
}

#[test]
fn hevc_frame_dimension_overflow_returns_an_error() {
    assert!(HeicFrame::try_with_params(u32::MAX, 2, 8, 1).is_err());
}

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn tracked<T>(run: impl FnOnce() -> T) -> (T, usize) {
    LARGEST_ALLOCATION.with(|largest| largest.set(Some(0)));
    let result = run();
    let largest = LARGEST_ALLOCATION.with(|largest| largest.replace(None).unwrap());
    (result, largest)
}

fn boxed(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [&((data.len() + 8) as u32).to_be_bytes()[..], kind, data].concat()
}

fn full_box(kind: &[u8; 4], version: u8, data: &[u8]) -> Vec<u8> {
    boxed(kind, &[&[version, 0, 0, 0], data].concat())
}

fn ispe(width: u32, height: u32) -> Vec<u8> {
    full_box(
        b"ispe",
        0,
        &[width.to_be_bytes(), height.to_be_bytes()].concat(),
    )
}

type FixtureItem = ((u32, u32), Vec<u8>, Vec<u8>);

fn image_fixture(
    kind: &[u8; 4],
    dimensions: (u32, u32),
    config: Vec<u8>,
    payload: Vec<u8>,
    auxiliary: Option<FixtureItem>,
) -> Vec<u8> {
    image_fixture_with_color(kind, dimensions, config, payload, auxiliary, None)
}

fn image_fixture_with_color(
    kind: &[u8; 4],
    dimensions: (u32, u32),
    config: Vec<u8>,
    payload: Vec<u8>,
    auxiliary: Option<FixtureItem>,
    icc: Option<Vec<u8>>,
) -> Vec<u8> {
    let count: u16 = if auxiliary.is_some() { 2 } else { 1 };
    let mut info = count.to_be_bytes().to_vec();
    let mut locations = [vec![0x44, 0], count.to_be_bytes().to_vec()].concat();
    let mut associations = u32::from(count).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let mut properties = Vec::new();
    let mut items = vec![(dimensions, config, payload)];
    if let Some(auxiliary) = auxiliary {
        items.push(auxiliary);
    }
    for (index, (dimensions, config, payload)) in items.into_iter().enumerate() {
        let id = index as u16 + 1;
        info.extend(full_box(
            b"infe",
            2,
            &[&id.to_be_bytes()[..], &[0, 0], kind, &[0]].concat(),
        ));
        locations.extend_from_slice(&id.to_be_bytes());
        locations.extend_from_slice(&[0, 1, 0, 0, 0, 1]);
        locations.extend_from_slice(&(data.len() as u32).to_be_bytes());
        locations.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend(payload);
        associations.extend_from_slice(&id.to_be_bytes());
        if id == 1 && icc.is_some() {
            associations.extend_from_slice(&[3, 0x81, 0x82, if count == 1 { 0x83 } else { 0x86 }]);
        } else {
            associations.extend_from_slice(if id == 1 {
                &[2, 0x81, 0x82]
            } else {
                &[3, 0x83, 0x84, 0x85]
            });
        }
        properties.extend(ispe(dimensions.0, dimensions.1));
        properties.extend(config);
    }
    if count == 2 {
        properties.extend(full_box(
            b"auxC",
            0,
            b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0",
        ));
    }
    if let Some(icc) = icc {
        properties.extend(boxed(b"colr", &[&b"prof"[..], &icc].concat()));
    }
    let mut meta = [
        full_box(b"pitm", 0, &1u16.to_be_bytes()),
        full_box(b"iinf", 0, &info),
        full_box(b"iloc", 1, &locations),
        boxed(
            b"iprp",
            &[
                boxed(b"ipco", &properties),
                full_box(b"ipma", 0, &associations),
            ]
            .concat(),
        ),
        boxed(b"idat", &data),
    ]
    .concat();
    if count == 2 {
        meta.extend(full_box(
            b"iref",
            0,
            &boxed(
                b"auxl",
                &[2u16.to_be_bytes(), 1u16.to_be_bytes(), 1u16.to_be_bytes()].concat(),
            ),
        ));
    }
    [
        boxed(
            b"ftyp",
            &[
                if kind == b"av01" {
                    &b"avif"[..]
                } else {
                    &b"heic"[..]
                },
                &[0, 0, 0, 0],
                b"mif1",
            ]
            .concat(),
        ),
        full_box(b"meta", 0, &meta),
    ]
    .concat()
}

fn hevc_configuration(sps: Vec<u8>) -> Vec<u8> {
    let mut config = vec![0; 23];
    config[0] = 1;
    config[1] = 1;
    config[16] = 1;
    config[17] = 1;
    config[18] = 0xf8;
    config[19] = 0xf8;
    config[21] = 3;
    config[22] = 3;
    for nal in [
        hex("40010c01ffff0408000003009fa800000300001eba0240"),
        sps,
        hex("4401c173c089"),
    ] {
        config.push((nal[0] >> 1) & 63);
        config.extend_from_slice(&1u16.to_be_bytes());
        config.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        config.extend(nal);
    }
    boxed(b"hvcC", &config)
}

fn small_hevc_sps() -> Vec<u8> {
    hex("4201010408000003009fa800000300001ea0884596eaaf2bc05a020000030002000003003210")
}

fn large_hevc_sps() -> Vec<u8> {
    hex("42010103700000030090000003000003001ea010202075596566924cae0100000303e800000303e808")
}

fn hevc_payload() -> Vec<u8> {
    let slice = hex("2801af78f85df7e170");
    [&(slice.len() as u32).to_be_bytes()[..], &slice].concat()
}

fn bit_reader_ue(bits: &[bool], position: &mut usize) -> u32 {
    let mut zeros = 0;
    while !bits[*position] {
        zeros += 1;
        *position += 1;
    }
    *position += 1;
    let mut value = 1;
    for _ in 0..zeros {
        value = (value << 1) | u32::from(bits[*position]);
        *position += 1;
    }
    value - 1
}

fn bit_writer_ue(bits: &mut Vec<bool>, value: u32) {
    let value = value + 1;
    let width = 32 - value.leading_zeros();
    bits.extend(std::iter::repeat_n(false, (width - 1) as usize));
    for i in (0..width).rev() {
        bits.push(value & (1 << i) != 0);
    }
}

fn hidden_geometry_sps() -> Vec<u8> {
    let base = small_hevc_sps();
    let sps = crate::heic_decoder::hevc::bitstream::parse_single_nal(&base).unwrap();
    let mut bits: Vec<bool> = sps
        .payload
        .iter()
        .flat_map(|byte| (0..8).rev().map(move |i| byte & (1 << i) != 0))
        .collect();
    let mut cursor = 104;
    bit_reader_ue(&bits, &mut cursor);
    assert_eq!(bit_reader_ue(&bits, &mut cursor), 1);
    let start = cursor;
    assert_eq!(bit_reader_ue(&bits, &mut cursor), 16);
    assert_eq!(bit_reader_ue(&bits, &mut cursor), 16);
    assert!(!bits[cursor]);
    cursor += 1;
    let mut replacement = Vec::new();
    bit_writer_ue(&mut replacement, 16384);
    bit_writer_ue(&mut replacement, 16384);
    replacement.push(true);
    for crop in [0, 8184, 0, 8184] {
        bit_writer_ue(&mut replacement, crop);
    }
    bits.splice(start..cursor, replacement);
    while !bits.len().is_multiple_of(8) {
        bits.push(false);
    }
    let raw: Vec<u8> = bits
        .chunks_exact(8)
        .map(|b| b.iter().fold(0, |v, &bit| (v << 1) | u8::from(bit)))
        .collect();
    let mut result = base[..2].to_vec();
    let mut zeros = 0;
    for byte in raw {
        if zeros >= 2 && byte <= 3 {
            result.push(3);
            zeros = 0;
        }
        result.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    result
}

fn junk_nal_with_hidden_parameter_sets(length: usize) -> Vec<u8> {
    let mut nal = [
        &[0x4eu8, 0x01][..],
        &[0, 0, 0, 1][..],
        &hidden_geometry_sps()[..],
        &[0, 0, 1][..],
        &hex("4401c173c089")[..],
    ]
    .concat();
    nal.resize(length, 0xff);
    nal
}

fn hevc_configuration_with_leading_nals(sps: Vec<u8>, leading: &[Vec<u8>]) -> Vec<u8> {
    let mut config = vec![0u8; 23];
    config[0] = 1;
    config[1] = 1;
    config[16] = 1;
    config[17] = 1;
    config[18] = 0xf8;
    config[19] = 0xf8;
    config[21] = 3;
    config[22] = (leading.len() + 3) as u8;
    for nal in leading.iter().chain([
        &hex("40010c01ffff0408000003009fa800000300001eba0240"),
        &sps,
        &hex("4401c173c089"),
    ]) {
        config.push((nal[0] >> 1) & 63);
        config.extend_from_slice(&1u16.to_be_bytes());
        config.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        config.extend_from_slice(nal);
    }
    boxed(b"hvcC", &config)
}

#[test]
fn aggregate_memory_hidden_parameter_sets_stay_within_budget() {
    let leading = vec![junk_nal_with_hidden_parameter_sets(300)];
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration_with_leading_nals(small_hevc_sps(), &leading),
        hevc_payload(),
        None,
    );
    let clean = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        None,
    );
    let (decoded, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(1024 * 1024),
        )
        .unwrap()
    });
    assert_eq!(decoded, decode_bytes_to_rgba(&clean).unwrap());
    assert!(
        largest < 1024 * 1024,
        "hidden parameter sets bypassed admission: {largest}"
    );
}

#[test]
fn hevc_decode_uses_fixed_length_prefixed_framing_at_first_nal_boundaries() {
    let clean = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        None,
    );
    let expected = decode_bytes_to_rgba(&clean).unwrap();
    for length in [1usize, 255, 256, 511, 512] {
        let mut junk = vec![0x4eu8, 0x01];
        junk.resize(length, 0xff);
        let mut leading = vec![junk];
        if length == 1 {
            leading.push(junk_nal_with_hidden_parameter_sets(300));
        }
        let input = image_fixture(
            b"hvc1",
            (16, 16),
            hevc_configuration_with_leading_nals(small_hevc_sps(), &leading),
            hevc_payload(),
            None,
        );
        let (decoded, largest) = tracked(|| {
            decode_bytes_to_rgba_with_options(
                &input,
                DecodeGuardrails::default().with_memory_budget(1024 * 1024),
            )
            .unwrap()
        });
        assert_eq!(decoded, expected, "first NAL length {length}");
        assert!(
            largest < 1024 * 1024,
            "first NAL length {length} bypassed admission: {largest}"
        );
    }
}

fn leading_alpha_configuration() -> Vec<Vec<u8>> {
    vec![junk_nal_with_hidden_parameter_sets(300)]
}

#[test]
fn aggregate_memory_hidden_parameter_sets_in_auxiliary_alpha_stay_within_budget() {
    let input = image_fixture_with_color(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        Some((
            (16, 16),
            hevc_configuration_with_leading_nals(small_hevc_sps(), &leading_alpha_configuration()),
            hevc_payload(),
        )),
        None,
    );
    let (decoded, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(1024 * 1024),
        )
        .unwrap()
    });
    assert_eq!(
        decoded,
        decode_bytes_to_rgba(&small_memory_fixture(true, None)).unwrap()
    );
    assert!(
        largest < 1024 * 1024,
        "hidden alpha parameter sets bypassed admission: {largest}"
    );
}

#[test]
fn grid_tile_streams_with_hidden_parameter_sets_decode_strictly() {
    let leading = leading_alpha_configuration();
    let config = hevc_configuration_with_leading_nals(small_hevc_sps(), &leading);
    let tile = isobmff::HeicGridTileItemData {
        item_id: 1,
        construction_method: 1,
        hvcc: isobmff::parse_boxes(&config).unwrap()[0]
            .parse_hvcc()
            .unwrap(),
        colr: Default::default(),
        transforms: vec![],
        payload: hevc_payload(),
    };
    let (decoded, largest) = tracked(|| crate::decode_heic_grid_tile_to_image(&tile).unwrap());
    assert_eq!((decoded.width, decoded.height), (16, 16));
    assert!(
        largest < 1024 * 1024,
        "hidden tile parameter sets bypassed admission: {largest}"
    );
}

#[test]
fn aggregate_memory_rejects_rgba_before_decoding() {
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        None,
    );
    let (result, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeOptions {
                max_memory_bytes: Some(4096),
                ..Default::default()
            },
        )
    });
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
    assert!(
        largest < 512,
        "entered decoder before memory check: {largest}"
    );
}

#[test]
fn aggregate_memory_rejects_rgb_before_decoding() {
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        None,
    );
    let result = decode_bytes_to_rgb8_with_options(
        &input,
        DecodeOptions {
            max_memory_bytes: Some(4096),
            ..Default::default()
        },
    );
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
}

fn small_memory_fixture(alpha: bool, icc: Option<Vec<u8>>) -> Vec<u8> {
    image_fixture_with_color(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        alpha.then(|| {
            (
                (16, 16),
                hevc_configuration(small_hevc_sps()),
                hevc_payload(),
            )
        }),
        icc,
    )
}

fn aggregate_estimate(input: &[u8]) -> u64 {
    let mut estimate = 4096;
    loop {
        match decode_bytes_to_rgba_with_options(
            input,
            DecodeOptions {
                max_memory_bytes: Some(estimate),
                ..Default::default()
            },
        ) {
            Ok(_) => return estimate,
            Err(DecodeError::Guardrail(DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                actual_bytes,
                ..
            })) => {
                assert!(actual_bytes > estimate);
                estimate = actual_bytes;
            }
            Err(error) => panic!("unexpected rejection: {error}"),
        }
    }
}

#[test]
fn aggregate_memory_exact_limit_preserves_output() {
    let input = small_memory_fixture(false, None);
    let estimate = aggregate_estimate(&input);
    let baseline = decode_bytes_to_rgba(&input).unwrap();
    let limited = decode_bytes_to_rgba_with_options(
        &input,
        DecodeOptions {
            max_memory_bytes: Some(estimate),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(baseline, limited);
    assert!(
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeOptions {
                max_memory_bytes: Some(estimate - 1),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn existing_guardrail_literals_and_function_signatures_remain_compatible() {
    let input = small_memory_fixture(false, None);
    let guardrails = DecodeGuardrails {
        max_input_bytes: Some(128 * 1024 * 1024),
        max_pixels: Some(64_000_000),
        max_temp_spool_bytes: Some(128 * 1024 * 1024),
        temp_spool_directory: None,
    };
    let decode: fn(&[u8], DecodeGuardrails) -> Result<DecodedRgbaImage, DecodeError> =
        decode_bytes_to_rgba_with_guardrails;
    assert!(decode(&input, guardrails).is_ok());
    assert!(decode_bytes_to_rgba_with_guardrails(&input, Default::default()).is_ok());
}

#[test]
fn aggregate_memory_includes_auxiliary_planes() {
    let opaque = small_memory_fixture(false, None);
    let alpha = small_memory_fixture(true, None);
    let opaque_estimate = aggregate_estimate(&opaque);
    assert!(aggregate_estimate(&alpha) > opaque_estimate);
    assert!(
        decode_bytes_to_rgba_with_options(
            &alpha,
            DecodeOptions {
                max_memory_bytes: Some(opaque_estimate),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(
        decode_bytes_to_rgba(&alpha).unwrap(),
        decode_bytes_to_rgba_with_options(
            &alpha,
            DecodeOptions {
                max_memory_bytes: Some(1024 * 1024),
                ..Default::default()
            }
        )
        .unwrap()
    );
}

#[test]
fn aggregate_memory_reserves_icc_and_rgb_conversion() {
    let icc = nclx_to_icc_profile(&isobmff::NclxColorProfile {
        colour_primaries: 12,
        transfer_characteristics: 13,
        matrix_coefficients: 1,
        full_range_flag: true,
    })
    .unwrap();
    let input = small_memory_fixture(false, Some(icc.clone()));
    let estimate = aggregate_estimate(&input);
    assert!(estimate > 16 * 1024 * 1024 + 16 * 16 * 14);
    let baseline = decode_bytes_to_rgba(&input).unwrap();
    let limited = decode_bytes_to_rgba_with_options(
        &input,
        DecodeOptions {
            max_memory_bytes: Some(estimate),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(baseline, limited);
    assert_eq!(limited.icc_profile, Some(icc));
}

#[test]
fn aggregate_memory_rejects_path_and_spooled_input() {
    let input = small_memory_fixture(false, None);
    let guardrails = DecodeOptions {
        max_memory_bytes: Some(4096),
        ..Default::default()
    };
    assert!(
        decode_read_to_rgba_with_options(std::io::Cursor::new(&input), guardrails.clone()).is_err()
    );
    let path = std::env::temp_dir().join(format!("heic-memory-{}.heic", std::process::id()));
    std::fs::write(&path, &input).unwrap();
    let result = decode_path_to_rgba_with_options(&path, guardrails);
    std::fs::remove_file(path).unwrap();
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
}

#[cfg(feature = "image-integration")]
#[test]
fn aggregate_memory_rejects_image_adapter_before_output_allocation() {
    let input = small_memory_fixture(false, None);
    let result = decode_bytes_to_rgba_layout_with_hint_and_guardrails(
        &input,
        None,
        DecodeOptions {
            max_memory_bytes: Some(4096),
            ..Default::default()
        },
    );
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
}

#[cfg(feature = "image-integration")]
#[test]
fn aggregate_memory_image_adapter_reuses_payload_and_preserves_pixels() {
    let input = small_memory_fixture(false, None);
    let options = DecodeOptions {
        max_memory_bytes: Some(1024 * 1024),
        ..Default::default()
    };
    let probe = decode_bytes_to_rgba_layout_with_hint_and_guardrails(&input, None, options.clone())
        .unwrap();
    assert!(probe.preextracted_heic.is_some());
    let mut pixels = vec![0; 16 * 16 * 4];
    decode_bytes_to_rgba8_slice_with_hint_and_guardrails(
        &input,
        None,
        options,
        probe.preextracted_heic,
        &mut pixels,
    )
    .unwrap();
    assert_eq!(
        decode_bytes_to_rgba(&input).unwrap().pixels,
        DecodedRgbaPixels::U8(pixels)
    );
}

#[test]
fn aggregate_memory_includes_grid_working_set() {
    let input = small_memory_fixture(false, None);
    let grid = grid_with_filler(4, 1024);
    let primary = isobmff::HeicPrimaryItemDataWithGrid::Grid(grid);
    let guardrails = DecodeOptions {
        max_memory_bytes: Some(100_000),
        ..Default::default()
    };
    let result = memory::heic(&input, &mut None, &primary, &guardrails, false, 0);
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
}

fn replace_meta_child(input: &[u8], kind: &[u8; 4], replacement: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    for parsed in isobmff::BoxIter::new(input) {
        let parsed = parsed.unwrap();
        let box_type = parsed.header.box_type.as_bytes();
        if box_type != *b"meta" {
            output.extend(boxed(&box_type, parsed.payload));
            continue;
        }
        let mut children = Vec::new();
        for child in parsed.parse_meta().unwrap().children() {
            let child = child.unwrap();
            let child_type = child.header.box_type.as_bytes();
            if child_type == *kind {
                children.extend_from_slice(replacement);
            } else {
                children.extend(boxed(&child_type, child.payload));
            }
        }
        output.extend(full_box(b"meta", 0, &children));
    }
    output
}

fn repeated_encoded_fixture(alpha: bool, repeats: u16) -> Vec<u8> {
    let slice = hevc_payload();
    let mut filler = vec![0xff; 128 * 1024];
    filler[..2].copy_from_slice(&[0x4c, 0x01]);
    *filler.last_mut().unwrap() = 0x80;
    let filler = [&(filler.len() as u32).to_be_bytes()[..], &filler].concat();
    let expanded = [&filler[..], &slice].concat();
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        if alpha {
            slice.clone()
        } else {
            expanded.clone()
        },
        alpha.then(|| ((16, 16), hevc_configuration(small_hevc_sps()), expanded)),
    );
    let boxes = isobmff::parse_boxes(&input).unwrap();
    let meta = find_first_box_by_type(&boxes, META_BOX_TYPE)
        .unwrap()
        .parse_meta()
        .unwrap();
    let resolved = meta.resolve_primary_item().unwrap();
    let mut locations = [
        vec![0x44, 0],
        (resolved.iloc.items.len() as u16).to_be_bytes().to_vec(),
    ]
    .concat();
    for item in &resolved.iloc.items {
        let repeated = item.item_id == if alpha { 2 } else { 1 };
        locations.extend_from_slice(&(item.item_id as u16).to_be_bytes());
        locations.extend_from_slice(&[0, 1, 0, 0]);
        locations.extend_from_slice(&(if repeated { repeats + 1 } else { 1 }).to_be_bytes());
        let offset = item.extents[0].offset as u32;
        if repeated {
            for _ in 0..repeats {
                locations.extend_from_slice(&offset.to_be_bytes());
                locations.extend_from_slice(&(filler.len() as u32).to_be_bytes());
            }
            locations.extend_from_slice(&(offset + filler.len() as u32).to_be_bytes());
            locations.extend_from_slice(&(slice.len() as u32).to_be_bytes());
        } else {
            locations.extend_from_slice(&offset.to_be_bytes());
            locations.extend_from_slice(&(slice.len() as u32).to_be_bytes());
        }
    }
    replace_meta_child(&input, b"iloc", &full_box(b"iloc", 1, &locations))
}

#[test]
fn aggregate_memory_rejects_expanded_alpha_before_materialization() {
    for repeats in [32, 200] {
        let input = repeated_encoded_fixture(true, repeats);
        assert!(decode_bytes_to_rgba(&input).is_ok());
        let (result, largest) = tracked(|| {
            decode_bytes_to_rgba_with_options(
                &input,
                DecodeGuardrails::default().with_memory_budget(1024 * 1024),
            )
        });
        assert!(matches!(
            result,
            Err(DecodeError::Guardrail(
                DecodeGuardrailError::DecodedByteCountExceeded {
                    stage: "aggregate decode",
                    ..
                }
            ))
        ));
        assert!(
            largest < 1024 * 1024,
            "alpha materialized before admission: {largest}"
        );
    }
}

#[test]
fn aggregate_memory_charges_retained_coded_payload_at_growth_boundaries() {
    for (repeats, limit) in [(33, 14 * 1024 * 1024), (65, 26 * 1024 * 1024)] {
        let input = repeated_encoded_fixture(false, repeats);
        let (result, largest) = tracked(|| {
            decode_bytes_to_rgba_with_options(
                &input,
                DecodeGuardrails::default().with_memory_budget(limit),
            )
        });
        assert!(result.is_err());
        assert!(
            largest < limit as usize,
            "coded payload allocated before admission: {largest}"
        );
    }
}

#[test]
fn aggregate_memory_checks_box_collections_before_parsing() {
    let mut input = small_memory_fixture(false, None);
    for _ in 0..10_000 {
        input.extend(boxed(b"free", &[]));
    }
    assert!(decode_bytes_to_rgba(&input).is_ok());
    let (result, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(300_000),
        )
    });
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
    assert!(
        largest < 300_000,
        "box collection allocated before admission: {largest}"
    );
}

#[test]
fn aggregate_memory_checks_zero_width_extent_tables_before_parsing() {
    let input = small_memory_fixture(false, None);
    let locations = [
        &[0, 0, 0, 1][..],
        &[0, 1, 0, 0, 0, 0][..],
        &u16::MAX.to_be_bytes(),
    ]
    .concat();
    let input = replace_meta_child(&input, b"iloc", &full_box(b"iloc", 1, &locations));
    let (result, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(1024 * 1024),
        )
    });
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
    assert!(
        largest < 4096,
        "zero-width table allocated before admission: {largest}"
    );
}

#[test]
fn aggregate_memory_checks_unused_location_tables_before_parsing() {
    let mut input = repeated_encoded_fixture(true, 50_000);
    let alpha_type = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha";
    let offset = input
        .windows(alpha_type.len())
        .position(|bytes| bytes == alpha_type)
        .unwrap();
    input[offset] = b'x';
    assert!(decode_bytes_to_rgba(&input).is_ok());
    let (result, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(1_700_000),
        )
    });
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
    assert!(
        largest < 100_000,
        "unused location table allocated before admission: {largest}"
    );
}

#[test]
fn aggregate_memory_checks_empty_configuration_units_before_parsing() {
    let config = hevc_configuration(small_hevc_sps());
    let mut payload = config[8..].to_vec();
    payload[22] += 1;
    payload.push(0);
    payload.extend_from_slice(&u16::MAX.to_be_bytes());
    payload.resize(payload.len() + usize::from(u16::MAX) * 2, 0);
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        boxed(b"hvcC", &payload),
        hevc_payload(),
        None,
    );
    assert!(decode_bytes_to_rgba(&input).is_ok());
    let (result, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(1_700_000),
        )
    });
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
    assert!(
        largest < 100_000,
        "configuration table allocated before admission: {largest}"
    );
}

fn repeated_grid_properties_fixture(empty_units: bool, rotations: usize) -> Vec<u8> {
    let mut config = hevc_configuration(small_hevc_sps())[8..].to_vec();
    if empty_units {
        config[22] += 1;
        config.push(0);
        config.extend_from_slice(&u16::MAX.to_be_bytes());
        config.resize(config.len() + usize::from(u16::MAX) * 2, 0);
    }
    let descriptor = [
        vec![0, 0, 0, 31],
        512u16.to_be_bytes().to_vec(),
        16u16.to_be_bytes().to_vec(),
    ]
    .concat();
    let payload = hevc_payload();
    let mut info = 2u16.to_be_bytes().to_vec();
    info.extend(full_box(
        b"infe",
        2,
        &[&[0, 1, 0, 0][..], b"grid\0"].concat(),
    ));
    info.extend(full_box(
        b"infe",
        2,
        &[&[0, 2, 0, 0][..], b"hvc1\0"].concat(),
    ));
    let mut locations = vec![0x44, 0, 0, 2];
    for (id, offset, length) in [
        (1u16, 0u32, descriptor.len()),
        (2, descriptor.len() as u32, payload.len()),
    ] {
        locations.extend_from_slice(&id.to_be_bytes());
        locations.extend_from_slice(&[0, 1, 0, 0, 0, 1]);
        locations.extend_from_slice(&offset.to_be_bytes());
        locations.extend_from_slice(&(length as u32).to_be_bytes());
    }
    let mut associations = (2u32 + rotations.div_ceil(255) as u32)
        .to_be_bytes()
        .to_vec();
    associations.extend_from_slice(&[0, 1, 1, 0x81, 0, 2, 2, 0x82, 0x83]);
    let mut remaining = rotations;
    while remaining > 0 {
        let count = remaining.min(255);
        associations.extend_from_slice(&[0, 2, count as u8]);
        associations.resize(associations.len() + count, 0x84);
        remaining -= count;
    }
    let mut properties = [ispe(512, 16), boxed(b"hvcC", &config), ispe(16, 16)].concat();
    if rotations > 0 {
        properties.extend(boxed(b"irot", &[0]));
    }
    let mut references = vec![0, 1, 0, 32];
    for _ in 0..32 {
        references.extend_from_slice(&2u16.to_be_bytes());
    }
    let meta = [
        full_box(b"pitm", 0, &1u16.to_be_bytes()),
        full_box(b"iinf", 0, &info),
        full_box(b"iloc", 1, &locations),
        boxed(
            b"iprp",
            &[
                boxed(b"ipco", &properties),
                full_box(b"ipma", 0, &associations),
            ]
            .concat(),
        ),
        full_box(b"iref", 0, &boxed(b"dimg", &references)),
        boxed(b"idat", &[descriptor, payload].concat()),
    ]
    .concat();
    [
        boxed(b"ftyp", b"heic\0\0\0\0mif1"),
        full_box(b"meta", 0, &meta),
    ]
    .concat()
}

#[test]
fn aggregate_memory_reserves_repeated_grid_codec_tables_and_transforms() {
    for (empty_units, rotations) in [(true, 0), (false, 50_000)] {
        let input = repeated_grid_properties_fixture(empty_units, rotations);
        let baseline = decode_bytes_to_rgba(&input).unwrap();
        let limit = 24 * 1024 * 1024;
        let (result, largest) = tracked(|| {
            decode_bytes_to_rgba_with_options(
                &input,
                DecodeGuardrails::default().with_memory_budget(limit),
            )
        });
        assert!(matches!(
            result,
            Err(DecodeError::Guardrail(
                DecodeGuardrailError::DecodedByteCountExceeded {
                    stage: "aggregate decode",
                    ..
                }
            ))
        ));
        assert!(
            largest < 2 * 1024 * 1024,
            "grid properties materialized before admission: {largest}"
        );
        let admitted = decode_bytes_to_rgba_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(512 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(baseline, admitted);
    }
}

pub(crate) fn linked_exif_candidates_fixture(count: u16) -> Vec<u8> {
    let input = small_memory_fixture(false, None);
    let payload = hevc_payload();
    let mut exif = hex("0000000049492a0008000000010012010300010000000600000000000000");
    exif.resize(32 * 1024, 0);
    let mut info = (count + 1).to_be_bytes().to_vec();
    info.extend(full_box(b"infe", 2, b"\0\x01\0\0hvc1\0"));
    let mut locations = [vec![0x44, 0], (count + 1).to_be_bytes().to_vec()].concat();
    let mut references = Vec::new();
    for id in 1..=count + 1 {
        locations.extend_from_slice(&id.to_be_bytes());
        locations.extend_from_slice(&[0, 1, 0, 0, 0, 1]);
        let (offset, length) = if id == 1 {
            (0, payload.len())
        } else {
            (payload.len(), exif.len())
        };
        locations.extend_from_slice(&(offset as u32).to_be_bytes());
        locations.extend_from_slice(&(length as u32).to_be_bytes());
        if id > 1 {
            info.extend(full_box(
                b"infe",
                2,
                &[&id.to_be_bytes()[..], b"\0\0Exif\0"].concat(),
            ));
            references.extend(boxed(
                b"cdsc",
                &[id.to_be_bytes(), 1u16.to_be_bytes(), 1u16.to_be_bytes()].concat(),
            ));
        }
    }
    let input = replace_meta_child(&input, b"iinf", &full_box(b"iinf", 0, &info));
    let input = replace_meta_child(&input, b"iloc", &full_box(b"iloc", 1, &locations));
    let input = replace_meta_child(&input, b"idat", &boxed(b"idat", &[payload, exif].concat()));
    let boxes = isobmff::parse_boxes(&input).unwrap();
    let meta = find_first_box_by_type(&boxes, META_BOX_TYPE).unwrap();
    let mut children = meta.payload[4..].to_vec();
    children.extend(full_box(b"iref", 0, &references));
    [
        boxed(b"ftyp", b"heic\0\0\0\0mif1"),
        full_box(b"meta", 0, &children),
    ]
    .concat()
}

#[test]
fn aggregate_memory_checks_exif_only_when_accessed() {
    let input = linked_exif_candidates_fixture(128);
    let options = DecodeGuardrails::default().with_memory_budget(1024 * 1024);
    assert!(decode_bytes_to_rgba_with_options(&input, options.clone()).is_ok());
    assert_eq!(primary_exif_orientation(&input), Some(6));
    let (hint, largest) = tracked(|| exif_orientation_hint_with_options(&input, options.clone()));
    assert!(hint.is_err());
    assert!(largest < 1024 * 1024);
    assert_eq!(
        exif_orientation_hint_with_options(
            &input,
            DecodeGuardrails::default().with_memory_budget(16 * 1024 * 1024)
        )
        .unwrap(),
        exif_orientation_hint(&input),
    );
    assert!(matches!(
        exif_orientation_hint_with_options(
            &input,
            DecodeOptions {
                max_input_bytes: Some(input.len() as u64 - 1),
                ..Default::default()
            }
        ),
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::InputTooLarge { .. }
        ))
    ));
    let (result, largest) = tracked(|| memory::validate_exif(&input, &None, &options, 0));
    assert!(result.is_err());
    assert!(
        largest < 1024 * 1024,
        "EXIF materialized before admission: {largest}"
    );
    let path = std::env::temp_dir().join(format!("heic-linked-exif-{}.heic", std::process::id()));
    std::fs::write(&path, &input).unwrap();
    let (result, largest) =
        tracked(|| exif_orientation_hint_from_path_with_options(&path, options));
    assert!(result.is_err());
    assert!(largest < 1024 * 1024);
    let hint = exif_orientation_hint_from_path_with_options(
        &path,
        DecodeGuardrails::default().with_memory_budget(16 * 1024 * 1024),
    )
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(hint.exif_orientation, Some(6));
}

pub(crate) fn unsupported_exif_candidate_fixture() -> Vec<u8> {
    let input = linked_exif_candidates_fixture(1);
    let payload_bytes = hevc_payload().len() as u32;
    let locations = [
        vec![0x44, 0, 0, 2, 0, 1, 0, 1, 0, 0, 0, 1],
        0u32.to_be_bytes().to_vec(),
        payload_bytes.to_be_bytes().to_vec(),
        vec![0, 2, 0, 2, 0, 0, 0, 1],
        payload_bytes.to_be_bytes().to_vec(),
        (8u32 * 1024 * 1024).to_be_bytes().to_vec(),
    ]
    .concat();
    replace_meta_child(&input, b"iloc", &full_box(b"iloc", 1, &locations))
}

#[test]
fn aggregate_memory_reserves_unsupported_exif_allocation_attempts() {
    let input = unsupported_exif_candidate_fixture();
    let options = DecodeGuardrails::default().with_memory_budget(1024 * 1024);
    assert!(decode_bytes_to_rgba_with_options(&input, options.clone()).is_ok());
    let (result, largest) = tracked(|| exif_orientation_hint_with_options(&input, options.clone()));
    assert!(result.is_err());
    assert!(
        largest < 1024 * 1024,
        "unsupported EXIF allocated before admission: {largest}"
    );
    let path =
        std::env::temp_dir().join(format!("heic-unsupported-exif-{}.heic", std::process::id()));
    std::fs::write(&path, &input).unwrap();
    let (result, largest) =
        tracked(|| exif_orientation_hint_from_path_with_options(&path, options));
    std::fs::remove_file(&path).unwrap();
    assert!(result.is_err());
    assert!(largest < 1024 * 1024);
}

#[test]
fn aggregate_memory_ignores_opaque_padding_copies() {
    let mut mdat = small_memory_fixture(false, None);
    let mut free = mdat.clone();
    let padding = vec![0; 64 * 1024];
    mdat.extend(boxed(b"mdat", &padding));
    free.extend(boxed(b"free", &padding));
    assert_eq!(aggregate_estimate(&mdat), aggregate_estimate(&free));
    let options = DecodeGuardrails::default().with_memory_budget(aggregate_estimate(&mdat));
    assert_eq!(
        decode_bytes_to_rgba_with_options(&mdat, options.clone()).unwrap(),
        decode_bytes_to_rgba_with_options(&free, options).unwrap()
    );
}

#[test]
fn aggregate_memory_rgb_ignores_unmaterialized_alpha_and_coded_dimg() {
    let input = repeated_encoded_fixture(true, 32);
    let options = DecodeGuardrails::default().with_memory_budget(1024 * 1024);
    assert_eq!(
        decode_bytes_to_rgb8(&input).unwrap(),
        decode_bytes_to_rgb8_with_options(&input, options.clone()).unwrap()
    );
    let input = replace_meta_child(
        &input,
        b"iref",
        &full_box(
            b"iref",
            0,
            &boxed(
                b"dimg",
                &[1u16.to_be_bytes(), 1u16.to_be_bytes(), 2u16.to_be_bytes()].concat(),
            ),
        ),
    );
    assert!(decode_bytes_to_rgb8_with_options(&input, options).is_ok());
}

#[test]
fn heic_alpha_rejects_coded_geometry_before_plane_allocation() {
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        Some((
            (16, 16),
            hevc_configuration(large_hevc_sps()),
            hevc_payload(),
        )),
    );
    let (alpha, largest) = tracked(|| {
        decode_primary_heic_auxiliary_alpha_plane_internal(
            &input,
            &mut None,
            16,
            16,
            &DecodeOptions {
                max_pixels: Some(256),
                ..Default::default()
            },
        )
    });
    assert!(alpha.unwrap().is_none());
    assert!(
        largest < 128 * 128 * 2,
        "allocated a frame plane: {largest}"
    );
}

fn grid_fixture() -> isobmff::HeicGridPrimaryItemData {
    let configuration = hevc_configuration(large_hevc_sps());
    let hvcc = isobmff::parse_boxes(&configuration).unwrap()[0]
        .parse_hvcc()
        .unwrap();
    isobmff::HeicGridPrimaryItemData {
        item_id: 2,
        construction_method: 1,
        descriptor: isobmff::HeicGridDescriptor {
            version: 0,
            rows: 1,
            columns: 1,
            output_width: 16,
            output_height: 16,
        },
        colr: Default::default(),
        tile_item_ids: vec![1],
        tiles: vec![isobmff::HeicGridTileItemData {
            item_id: 1,
            construction_method: 1,
            hvcc,
            colr: Default::default(),
            transforms: vec![],
            payload: hevc_payload(),
        }],
    }
}

#[test]
fn heic_grid_rejects_oversized_first_tile_before_plane_allocation() {
    let grid = grid_fixture();
    let (result, largest) = tracked(|| decode_primary_heic_grid_to_image(&grid));
    assert!(result.is_err());
    assert!(
        largest < 128 * 128 * 2,
        "allocated a frame plane: {largest}"
    );
}

fn av1_payload() -> Vec<u8> {
    hex(
        "0a0a191ebffff8640808181032a6051230034d186081fe024bbd7cdc902b8d7b2198a3c4d5957f8508779db412f630d58e2f586aee73c279733d70bbf79a6c3f7cd8dc5bc4c62982cecea9341bfdf6063be969c9d9f69d44f40479d0c313bf993c5cc6c29f0ed3abd114e8533d14a3794ee5bf9fb04e2bc144eb17a66ff12e3c5032112ae0e9f1cfec19876265cd63b6311d94a598b40b4d6e88ef403777276cc936fea2ca79f20aeecf9c07f34ce7e99a4d60bb344c5f7622fe11824045f5a7e99d6dc9a0a52ee9b60849ca2610949e602f8f905b0a474aed603aa4f6fdf14dc25499ed41a095bde98727b494c94f90c75a6d42dc39bcadc1f0dea9229010ef7ff9121d2caac0b25f0d7b748b3261a94f03f8fe32a7cb35e99a8875ccc36620b42e8d348b0509ca325cac66a78b9928aafb6156a490527f0a4f1f65606d05d61c563cd4ab259d33a5985cb04f997e5afeb67ac0dab782b02ee244feb8ca1c02338ec5779c35b9296ef437da4d7b215f1750ecda87494c2f9060ced8e6ce48e4a27ca77b17be83d652b8ed151d377fe1146a33c70559948e805da49ee8458fd6e2aa0dfc91b41f81fc066f926d71513c4763121826e9379837ce290c57c50cca91c69bd279270f6c2894d2a674625975cedff2be70809bde67a4fc29dfe1936a97d8e2ac8524c647f556b9115d74d4f385a42161ab5fc914958c04239b8a5453339e195c08d25acce7f0f7db23299fab27349c78973b1872b2eb25fd121806a7aaff2a95c2bbd1e85ac8c29346e76e10921c1d0cc0b90da9a8e96c1e3278fec1319044ffff1b0c9245e800fc2c332ac1f815d963d671f859a2985ad56bef7961cb6716e5f95695d2c060d4438c354afc1d5544a4ba3216029091c7e59dd254e461714c48f7a9248c97af44fca436dc8119ad130815c9ab878ec2f700434928e0f8a424768164f0d9368621bd480e58373a2e57ef53",
    )
}

#[test]
fn avif_primary_rejects_coded_geometry_before_plane_allocation() {
    let input = image_fixture(
        b"av01",
        (16, 16),
        boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
        av1_payload(),
        None,
    );
    let (result, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeOptions {
                max_pixels: Some(256),
                ..Default::default()
            },
        )
    });
    assert!(result.is_err());
    assert!(largest < 256 * 2048, "allocated a frame plane: {largest}");
}

#[test]
fn avif_alpha_rejects_coded_geometry_before_plane_allocation() {
    let input = image_fixture(
        b"av01",
        (16, 16),
        boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
        av1_payload(),
        Some((
            (16, 16),
            boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
            av1_payload(),
        )),
    );
    let (meta, resolved) = isobmff::resolve_primary_avif_item_graph(&input).unwrap();
    let (alpha, largest) = tracked(|| {
        decode_primary_avif_auxiliary_alpha_plane(
            &input,
            &mut None,
            &meta,
            &resolved,
            16,
            16,
            Some(256),
        )
    });
    assert!(alpha.is_none());
    assert!(largest < 256 * 2048, "allocated a frame plane: {largest}");
}

#[test]
fn heic_matching_alpha_is_preserved() {
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        Some((
            (16, 16),
            hevc_configuration(small_hevc_sps()),
            hevc_payload(),
        )),
    );
    let alpha = decode_primary_heic_auxiliary_alpha_plane_internal(
        &input,
        &mut None,
        16,
        16,
        &DecodeOptions {
            max_pixels: Some(256),
            ..Default::default()
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        (alpha.width, alpha.height, alpha.samples.len()),
        (16, 16, 256)
    );
}

#[test]
fn avif_matching_alpha_is_preserved() {
    let input = image_fixture(
        b"av01",
        (256, 2048),
        boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
        av1_payload(),
        Some((
            (256, 2048),
            boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
            av1_payload(),
        )),
    );
    let (meta, resolved) = isobmff::resolve_primary_avif_item_graph(&input).unwrap();
    let alpha = decode_primary_avif_auxiliary_alpha_plane(
        &input,
        &mut None,
        &meta,
        &resolved,
        256,
        2048,
        Some(256 * 2048),
    )
    .unwrap();
    assert_eq!((alpha.width, alpha.height), (256, 2048));
    assert!(
        matches!(alpha.samples, AvifPlaneSamples::U8(ref samples) if samples.len() == 256 * 2048)
    );
}

#[test]
fn avif_zero_pixel_limit_cannot_disable_backend_limit() {
    let input = image_fixture(
        b"av01",
        (16, 16),
        boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
        av1_payload(),
        None,
    );
    let (result, largest) = tracked(|| {
        decode_bytes_to_rgba_with_options(
            &input,
            DecodeOptions {
                max_pixels: Some(0),
                ..Default::default()
            },
        )
    });
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::PixelCountExceeded { max_pixels: 0, .. }
        ))
    ));
    assert!(largest < 256 * 2048);
}

#[test]
fn avif_auxiliary_container_geometry_is_checked_before_backend() {
    let input = image_fixture(
        b"av01",
        (16, 16),
        boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
        av1_payload(),
        Some((
            (256, 2048),
            boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
            av1_payload(),
        )),
    );
    let (meta, resolved) = isobmff::resolve_primary_avif_item_graph(&input).unwrap();
    let (alpha, largest) = tracked(|| {
        decode_primary_avif_auxiliary_alpha_plane(
            &input,
            &mut None,
            &meta,
            &resolved,
            16,
            16,
            Some(256),
        )
    });
    assert!(alpha.is_none());
    assert!(
        largest < 4096,
        "entered backend before container check: {largest}"
    );
}

#[test]
fn heic_grid_tile_crop_cannot_hide_oversized_coded_frame() {
    let mut grid = grid_fixture();
    grid.tiles[0]
        .transforms
        .push(isobmff::PrimaryItemTransformProperty::CleanAperture(
            isobmff::ImageCleanApertureProperty {
                clean_aperture_width_num: 16,
                clean_aperture_width_den: 1,
                clean_aperture_height_num: 16,
                clean_aperture_height_den: 1,
                horizontal_offset_num: 0,
                horizontal_offset_den: 1,
                vertical_offset_num: 0,
                vertical_offset_den: 1,
            },
        ));
    let (result, largest) = tracked(|| decode_primary_heic_grid_to_image(&grid));
    assert!(result.is_err());
    assert!(
        largest < 128 * 128 * 2,
        "tile crop hid coded frame allocation: {largest}"
    );
}

#[test]
fn heic_grid_checks_later_tiles_before_decoding_first_tile() {
    let mut grid = grid_fixture();
    grid.descriptor.columns = 2;
    grid.descriptor.output_width = 256;
    grid.descriptor.output_height = 126;
    let big = hex(
        "420101037000000300900000030000030078a007d200539f596ea4929ae6e021a0c08000000c800000030084",
    );
    let config = hevc_configuration(big);
    let mut second = grid.tiles[0].clone();
    second.item_id = 3;
    second.hvcc = isobmff::parse_boxes(&config).unwrap()[0]
        .parse_hvcc()
        .unwrap();
    grid.tile_item_ids.push(3);
    grid.tiles.push(second);
    let (result, largest) = tracked(|| decode_primary_heic_grid_to_image(&grid));
    assert!(result.is_err());
    assert!(
        largest < 128 * 128 * 2,
        "first tile decoded before later preflight: {largest}"
    );
}

#[test]
fn heic_grid_checks_replacement_sps_in_payload() {
    let mut grid = grid_fixture();
    let config = hevc_configuration(small_hevc_sps());
    grid.tiles[0].hvcc = isobmff::parse_boxes(&config).unwrap()[0]
        .parse_hvcc()
        .unwrap();
    let replacement = large_hevc_sps();
    grid.tiles[0].payload = [
        &(replacement.len() as u32).to_be_bytes()[..],
        &replacement,
        &hevc_payload(),
    ]
    .concat();
    let (result, largest) = tracked(|| decode_primary_heic_grid_to_image(&grid));
    assert!(result.is_err());
    assert!(largest < 128 * 128 * 2);
}

#[test]
fn heic_grid_preflight_alignment_does_not_overflow() {
    let mut grid = grid_fixture();
    grid.descriptor.output_width = u32::MAX;
    grid.descriptor.output_height = u32::MAX;
    assert!(geometry::preflight_grid(&grid, &DecodeOptions::default()).is_ok());
}

#[test]
fn heic_auxiliary_allocation_failure_is_propagated() {
    let input = image_fixture(
        b"hvc1",
        (126, 126),
        hevc_configuration(large_hevc_sps()),
        hevc_payload(),
        Some((
            (126, 126),
            hevc_configuration(large_hevc_sps()),
            hevc_payload(),
        )),
    );
    FAIL_ALLOCATION_AT.with(|limit| limit.set(Some(128 * 128 * 2)));
    let result = decode_primary_heic_auxiliary_alpha_plane_internal(
        &input,
        &mut None,
        126,
        126,
        &DecodeOptions::default(),
    );
    FAIL_ALLOCATION_AT.with(|limit| limit.set(None));
    assert!(matches!(
        result,
        Err(DecodeHeicError::AllocationFailed { .. })
    ));
}

#[test]
fn heic_copied_plane_allocation_failure_is_a_resource_error() {
    let source = vec![0u16; 256];
    FAIL_ALLOCATION_AT.with(|limit| limit.set(Some(512)));
    let result = extract_cropped_heic_plane(&source, 16, 0, 0, 16, 16, "alpha");
    FAIL_ALLOCATION_AT.with(|limit| limit.set(None));
    assert!(matches!(
        result,
        Err(DecodeHeicError::AllocationFailed { .. })
    ));
}

#[test]
fn heic_alpha_checks_decoded_bytes_before_allocating_planes() {
    let config = hevc_configuration(small_hevc_sps());
    let hvcc = isobmff::parse_boxes(&config).unwrap()[0]
        .parse_hvcc()
        .unwrap();
    let oversized_nal = vec![0x7fu8; 40 * 1024 * 1024];
    let payload = [
        &(oversized_nal.len() as u32).to_be_bytes()[..],
        &oversized_nal,
    ]
    .concat();
    let result = geometry::preflight_hevc(
        &hvcc,
        &payload,
        Some((16, 16)),
        &DecodeOptions {
            max_pixels: Some(256),
            ..Default::default()
        },
    );
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded { .. }
        ))
    ));
}

fn grid_with_filler(tile_count: u16, filler_bytes: usize) -> isobmff::HeicGridPrimaryItemData {
    let mut grid = grid_fixture();
    let config = hevc_configuration(small_hevc_sps());
    grid.tiles[0].hvcc = isobmff::parse_boxes(&config).unwrap()[0]
        .parse_hvcc()
        .unwrap();
    let mut filler = vec![0xffu8; filler_bytes];
    filler[..2].copy_from_slice(&[0x4c, 0x01]);
    *filler.last_mut().unwrap() = 0x80;
    grid.tiles[0].payload = [
        &(filler.len() as u32).to_be_bytes()[..],
        &filler,
        &hevc_payload(),
    ]
    .concat();
    grid.descriptor.columns = tile_count;
    grid.descriptor.output_width = 16 * u32::from(tile_count);
    for index in 1..tile_count {
        let mut tile = grid.tiles[0].clone();
        tile.item_id = u32::from(index) + 3;
        grid.tile_item_ids.push(tile.item_id);
        grid.tiles.push(tile);
    }
    grid
}

#[test]
fn heic_grid_first_tile_uses_decoded_byte_budget() {
    let grid = grid_with_filler(1, 24 * 1024 * 1024);
    let result = geometry::preflight_grid(&grid, &DecodeOptions::default());
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "HEVC tile",
                ..
            }
        ))
    ));
}

#[test]
fn heic_grid_aggregate_budget_includes_every_tile() {
    let grid = grid_with_filler(2, 12 * 1024 * 1024);
    let result = geometry::preflight_grid(&grid, &DecodeOptions::default());
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "HEIC grid",
                ..
            }
        ))
    ));
}

#[test]
fn avif_coded_geometry_mismatch_is_checked_without_a_pixel_limit() {
    let input = image_fixture(
        b"av01",
        (16, 16),
        boxed(b"av1C", &[0x81, 0x04, 0x0c, 0]),
        av1_payload(),
        None,
    );
    let (result, largest) =
        tracked(|| decode_bytes_to_rgba_with_options(&input, DecodeOptions::default()));
    assert!(matches!(
        result,
        Err(DecodeError::AvifDecode(
            DecodeAvifError::DecodedGeometryMismatch {
                expected_width: 16,
                expected_height: 16,
                actual_width: 256,
                actual_height: 2048
            }
        ))
    ));
    assert!(largest < 256 * 2048);
}

#[test]
fn aggregate_memory_rejects_png_operation_before_file_creation() {
    let input = small_memory_fixture(false, None);
    let path = std::env::temp_dir().join(format!("heic-png-admission-{}.png", std::process::id()));
    let (result, largest) = tracked(|| {
        decode_bytes_to_png_with_options(
            &input,
            &path,
            DecodeGuardrails::default().with_memory_budget(128 * 1024),
        )
    });
    assert!(matches!(
        result,
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded {
                stage: "aggregate decode",
                ..
            }
        ))
    ));
    assert!(!path.exists(), "output file created before admission");
    assert!(
        largest < 128 * 1024,
        "png encoding allocated before admission: {largest}"
    );
}

fn png_admission_charge(decoded: &DecodedRgbaImage, path: &Path) -> u64 {
    match write_decoded_rgba_to_png_with_options(
        decoded,
        path,
        DecodeGuardrails::default().with_memory_budget(1),
    ) {
        Err(DecodeError::Guardrail(DecodeGuardrailError::DecodedByteCountExceeded {
            actual_bytes,
            ..
        })) => actual_bytes,
        other => panic!("expected admission rejection, got {other:?}"),
    }
}

fn high_entropy_samples(count: usize) -> Vec<u16> {
    let mut samples = Vec::with_capacity(count);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        samples.push((state >> 48) as u16);
    }
    samples
}

fn png_writer_live_peak_case(decoded: DecodedRgbaImage, label: &str) {
    let path = std::env::temp_dir().join(format!(
        "heic-png-writer-{label}-{}.png",
        std::process::id()
    ));
    let charge = png_admission_charge(&decoded, &path);
    let _ = std::fs::remove_file(&path);
    LIVE_PEAK.with(|peak| peak.set(0));
    write_decoded_rgba_to_png_with_options(
        &decoded,
        &path,
        DecodeGuardrails::default().with_memory_budget(charge),
    )
    .unwrap_or_else(|err| panic!("{label}: admitted charge {charge} rejected: {err}"));
    let _ = std::fs::remove_file(&path);
    let peak = LIVE_PEAK.with(|peak| peak.get());
    assert!(
        peak <= charge as usize,
        "{label}: png writer live peak {peak} exceeded reservation {charge}"
    );
}

#[test]
fn png_writer_live_peak_stays_within_reservation() {
    png_writer_live_peak_case(
        DecodedRgbaImage {
            width: 1024,
            height: 1024,
            pixels: DecodedRgbaPixels::U16(high_entropy_samples(1024 * 1024 * 4)),
            icc_profile: None,
            source_bit_depth: 16,
        },
        "1024x1024-u16",
    );
    png_writer_live_peak_case(
        DecodedRgbaImage {
            width: 131072,
            height: 1,
            pixels: DecodedRgbaPixels::U16(high_entropy_samples(131072 * 4)),
            icc_profile: None,
            source_bit_depth: 16,
        },
        "131072x1-u16",
    );
    let mut icc = Vec::with_capacity(4 * 1024 * 1024);
    let mut state = 0x853c_49e6_748f_fea9u64;
    for _ in 0..4 * 1024 * 1024 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        icc.push((state >> 56) as u8);
    }
    png_writer_live_peak_case(
        DecodedRgbaImage {
            width: 1,
            height: 1,
            pixels: DecodedRgbaPixels::U8(vec![10, 20, 30, 255]),
            icc_profile: Some(icc),
            source_bit_depth: 8,
        },
        "1x1-large-icc",
    );
    png_writer_live_peak_case(
        DecodedRgbaImage {
            width: 512,
            height: 512,
            pixels: DecodedRgbaPixels::U16(high_entropy_samples(512 * 512 * 4)),
            icc_profile: None,
            source_bit_depth: 10,
        },
        "512x512-u16-10bit",
    );
}

#[test]
fn png_sixteen_bit_output_masks_replicated_low_bits() {
    let raw: Vec<u16> = (0..16 * 16 * 4)
        .map(|i| {
            if i % 4 == 3 {
                u16::MAX
            } else {
                0x83ffu16 + i as u16
            }
        })
        .collect();
    let masked: Vec<u16> = raw
        .iter()
        .enumerate()
        .map(|(i, &v)| if i % 4 == 3 { v } else { v & 0xffc0 })
        .collect();
    let with_depth = DecodedRgbaImage {
        width: 16,
        height: 16,
        pixels: DecodedRgbaPixels::U16(raw),
        icc_profile: None,
        source_bit_depth: 10,
    };
    let pre_masked = DecodedRgbaImage {
        width: 16,
        height: 16,
        pixels: DecodedRgbaPixels::U16(masked),
        icc_profile: None,
        source_bit_depth: 16,
    };
    let path_masked =
        std::env::temp_dir().join(format!("heic-png-mask-{}.png", std::process::id()));
    let path_control =
        std::env::temp_dir().join(format!("heic-png-control-{}.png", std::process::id()));
    write_decoded_rgba_to_png_with_options(
        &with_depth,
        &path_masked,
        DecodeGuardrails::default().with_memory_budget(1024 * 1024),
    )
    .unwrap();
    write_decoded_rgba_to_png_with_options(
        &pre_masked,
        &path_control,
        DecodeGuardrails::default().with_memory_budget(1024 * 1024),
    )
    .unwrap();
    let masked_bytes = std::fs::read(&path_masked).unwrap();
    let control_bytes = std::fs::read(&path_control).unwrap();
    let _ = std::fs::remove_file(&path_masked);
    let _ = std::fs::remove_file(&path_control);
    assert_eq!(masked_bytes, control_bytes);
}

#[test]
fn png_admission_charges_pixel_vector_capacity() {
    let mut samples: Vec<u16> = Vec::with_capacity(1_048_576);
    samples.push(0x1234);
    samples.push(0x5678);
    samples.push(0x9abc);
    samples.push(0xffff);
    let decoded = DecodedRgbaImage {
        width: 1,
        height: 1,
        pixels: DecodedRgbaPixels::U16(samples),
        icc_profile: None,
        source_bit_depth: 16,
    };
    let path = std::env::temp_dir().join(format!("heic-png-capacity-{}.png", std::process::id()));
    let charge = png_admission_charge(&decoded, &path);
    assert!(
        charge >= 2_097_152,
        "retained pixel capacity not charged: {charge}"
    );
    let _ = std::fs::remove_file(&path);
    LIVE_PEAK.with(|peak| peak.set(0));
    write_decoded_rgba_to_png_with_options(
        &decoded,
        &path,
        DecodeGuardrails::default().with_memory_budget(charge),
    )
    .unwrap();
    let _ = std::fs::remove_file(&path);
    let peak = LIVE_PEAK.with(|peak| peak.get());
    assert!(
        peak <= charge as usize,
        "capacity-inflated writer peak {peak} exceeded charge {charge}"
    );
}

#[cfg(feature = "image-integration")]
#[test]
fn aggregate_memory_reserves_icc_on_rgb8_and_layout_routes() {
    let icc = nclx_to_icc_profile(&isobmff::NclxColorProfile {
        colour_primaries: 12,
        transfer_characteristics: 13,
        matrix_coefficients: 1,
        full_range_flag: true,
    })
    .unwrap();
    let input = small_memory_fixture(false, Some(icc));
    let opaque_estimate = aggregate_estimate(&small_memory_fixture(false, None));
    let options = DecodeOptions {
        max_memory_bytes: Some(opaque_estimate + 17 * 1024 * 1024),
        ..Default::default()
    };
    assert!(decode_bytes_to_rgb8_with_options(&input, options.clone()).is_ok());
    let probe = decode_bytes_to_rgba_layout_with_hint_and_guardrails(&input, None, options)
        .expect("layout probe must admit the ICC reservation");
    assert!(probe.preextracted_heic.is_some());
    let tight = DecodeOptions {
        max_memory_bytes: Some(opaque_estimate + 1024),
        ..Default::default()
    };
    assert!(decode_bytes_to_rgb8_with_options(&input, tight.clone()).is_err());
    assert!(matches!(
        decode_bytes_to_rgba_layout_with_hint_and_guardrails(&input, None, tight),
        Err(DecodeError::Guardrail(
            DecodeGuardrailError::DecodedByteCountExceeded { .. }
        ))
    ));
}

#[test]
fn for_platform_presets_host_limits() {
    let options = DecodeOptions::for_platform();
    if cfg!(any(target_os = "android", target_os = "ios")) {
        assert_eq!(options.max_memory_bytes, Some(256 * 1024 * 1024));
        assert_eq!(options.max_pixels, Some(64_000_000));
    } else {
        assert_eq!(options.max_memory_bytes, Some(512 * 1024 * 1024));
        assert_eq!(options.max_pixels, Some(256_000_000));
    }
    assert_eq!(options.max_input_bytes, Some(128 * 1024 * 1024));
    assert_eq!(options.max_temp_spool_bytes, Some(128 * 1024 * 1024));
}

fn sps_heavy_payload(tail_repeats: usize) -> Vec<u8> {
    let mut sps = small_hevc_sps();
    let tail = [0x00u8, 0x00, 0x03];
    for _ in 0..tail_repeats {
        sps.extend_from_slice(&tail);
    }
    [&(sps.len() as u32).to_be_bytes()[..], &sps, &hevc_payload()].concat()
}

fn admitted_path_peak(input: Vec<u8>, name: &str) {
    let path =
        std::env::temp_dir().join(format!("heic-scratch-{name}-{}.heic", std::process::id()));
    std::fs::write(&path, &input).unwrap();
    drop(input);
    let mut budget = 4096u64;
    let charge = loop {
        let result = decode_path_to_rgba_with_options(
            &path,
            DecodeOptions {
                max_memory_bytes: Some(budget),
                ..Default::default()
            },
        );
        match result {
            Ok(decoded) => {
                drop(decoded);
                break budget;
            }
            Err(DecodeError::Guardrail(DecodeGuardrailError::DecodedByteCountExceeded {
                actual_bytes,
                ..
            })) => budget = actual_bytes,
            Err(error) => panic!("{name}: unexpected rejection: {error}"),
        }
    };
    LIVE_PEAK.with(|peak| peak.set(0));
    decode_path_to_rgba_with_options(
        &path,
        DecodeOptions {
            max_memory_bytes: Some(charge),
            ..Default::default()
        },
    )
    .unwrap_or_else(|error| panic!("{name}: admitted charge {charge} rejected: {error}"));
    std::fs::remove_file(&path).unwrap();
    let peak = LIVE_PEAK.with(|peak| peak.get());
    assert!(
        peak <= charge as usize,
        "{name}: parse live peak {peak} exceeded admitted charge {charge}"
    );
}

#[test]
fn aggregate_parse_scratch_stays_admitted_on_plain_extent_routes() {
    let payload = sps_heavy_payload(300_000);
    let primary = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        payload.clone(),
        None,
    );
    admitted_path_peak(primary, "primary");
    let alpha = image_fixture_with_color(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        Some(((16, 16), hevc_configuration(small_hevc_sps()), payload)),
        None,
    );
    admitted_path_peak(alpha, "alpha");
}

#[test]
fn aggregate_parse_scratch_stays_admitted_on_grid_tiles() {
    let mut grid = grid_fixture();
    let config = hevc_configuration(small_hevc_sps());
    grid.tiles[0].hvcc = isobmff::parse_boxes(&config).unwrap()[0]
        .parse_hvcc()
        .unwrap();
    grid.tiles[0].payload = sps_heavy_payload(200_000);
    let second = isobmff::HeicGridTileItemData {
        item_id: 2,
        construction_method: 1,
        hvcc: grid.tiles[0].hvcc.clone(),
        colr: Default::default(),
        transforms: vec![],
        payload: hevc_payload(),
    };
    grid.tiles.push(second);
    grid.tile_item_ids.push(2);
    grid.descriptor.columns = 2;
    grid.descriptor.output_width = 32;
    let input = image_fixture(
        b"hvc1",
        (16, 16),
        hevc_configuration(small_hevc_sps()),
        hevc_payload(),
        None,
    );
    let (result, largest) = tracked(|| {
        memory::heic(
            &input,
            &mut None,
            &isobmff::HeicPrimaryItemDataWithGrid::Grid(grid),
            &DecodeOptions {
                max_memory_bytes: Some(512 * 1024 * 1024),
                ..Default::default()
            },
            false,
            0,
        )
    });
    assert!(result.is_ok(), "grid parse estimate rejected: {result:?}");
    assert!(
        largest < 16 * 1024 * 1024,
        "grid tile parse materialized beyond estimate: {largest}"
    );
}
