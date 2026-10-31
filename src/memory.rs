use crate::*;
use std::collections::HashMap;

pub(super) const INPUT_STORAGE_COPIES: u64 = 3;
const METADATA_BOX_STORAGE_BYTES: u64 = 512;
const METADATA_PAYLOAD_COPIES: u64 = 8;
const METADATA_TABLE_COPIES: u64 = 4;
const PNG_ENCODER_BYTES: u64 = 512 * 1024;

fn read_field(payload: &[u8], cursor: &mut usize, width: usize) -> Option<u64> {
    let end = cursor.checked_add(width)?;
    let bytes = payload.get(*cursor..end)?;
    *cursor = end;
    Some(
        bytes
            .iter()
            .fold(0, |value, byte| (value << 8) | u64::from(*byte)),
    )
}

fn location_table_bytes(payload: &[u8]) -> Option<u64> {
    let version = *payload.first()?;
    if version > 2 {
        return None;
    }
    let sizes = payload.get(4..6)?;
    let offset_size = usize::from(sizes[0] >> 4);
    let length_size = usize::from(sizes[0] & 15);
    let base_size = usize::from(sizes[1] >> 4);
    let index_size = if version == 0 {
        0
    } else {
        usize::from(sizes[1] & 15)
    };
    if [offset_size, length_size, base_size, index_size]
        .iter()
        .any(|size| !matches!(size, 0 | 4 | 8))
    {
        return None;
    }
    let mut cursor = 6;
    let id_size = if version < 2 { 2 } else { 4 };
    let count = read_field(payload, &mut cursor, id_size)?;
    let mut bytes = count.saturating_mul(size_of::<isobmff::ItemLocationItem>() as u64);
    for _ in 0..count {
        if read_field(payload, &mut cursor, id_size).is_none()
            || (version > 0 && read_field(payload, &mut cursor, 2).is_none())
            || read_field(payload, &mut cursor, 2).is_none()
            || read_field(payload, &mut cursor, base_size).is_none()
        {
            break;
        }
        let Some(extents) = read_field(payload, &mut cursor, 2) else {
            break;
        };
        bytes = bytes.saturating_add(
            extents.saturating_mul(size_of::<isobmff::ItemLocationExtent>() as u64),
        );
        let extent_width = offset_size + length_size + index_size;
        let Some(end) = cursor.checked_add((extents as usize).saturating_mul(extent_width)) else {
            break;
        };
        if end > payload.len() {
            break;
        }
        cursor = end;
    }
    Some(bytes.saturating_mul(METADATA_TABLE_COPIES))
}

fn association_table_bytes(payload: &[u8]) -> Option<u64> {
    let version = *payload.first()?;
    if version > 1 {
        return None;
    }
    let wide = payload.get(3)? & 1 != 0;
    let mut cursor = 4;
    let count = read_field(payload, &mut cursor, 4)?;
    let mut bytes = count.saturating_mul(size_of::<isobmff::ItemPropertyAssociationEntry>() as u64);
    for _ in 0..count {
        if read_field(payload, &mut cursor, if version == 0 { 2 } else { 4 }).is_none() {
            break;
        }
        let Some(associations) = read_field(payload, &mut cursor, 1) else {
            break;
        };
        bytes = bytes.saturating_add(
            associations
                .saturating_mul(size_of::<isobmff::ResolvedPrimaryItemProperty<'static>>() as u64),
        );
        let Some(end) = cursor.checked_add(associations as usize * if wide { 2 } else { 1 }) else {
            break;
        };
        if end > payload.len() {
            break;
        }
        cursor = end;
    }
    Some(bytes.saturating_mul(METADATA_TABLE_COPIES))
}

fn hevc_configuration_table_bytes(payload: &[u8]) -> Option<u64> {
    let arrays = u64::from(*payload.get(22)?);
    let mut bytes = arrays.saturating_mul(size_of::<isobmff::HevcNalArray>() as u64);
    let mut cursor = 23;
    for _ in 0..arrays {
        if read_field(payload, &mut cursor, 1).is_none() {
            break;
        }
        let Some(units) = read_field(payload, &mut cursor, 2) else {
            break;
        };
        bytes = bytes.saturating_add(units.saturating_mul(size_of::<Vec<u8>>() as u64));
        for _ in 0..units {
            let Some(length) = read_field(payload, &mut cursor, 2) else {
                return Some(bytes);
            };
            let Some(end) = cursor.checked_add(length as usize) else {
                return Some(bytes);
            };
            if end > payload.len() {
                return Some(bytes);
            }
            cursor = end;
        }
    }
    Some(bytes)
}

fn box_metadata_bytes(parsed: &isobmff::ParsedBox<'_>, depth: u8) -> u64 {
    let kind = parsed.header.box_type.as_bytes();
    let payload = parsed.payload;
    let mut bytes = METADATA_BOX_STORAGE_BYTES;
    if matches!(&kind, b"mdat" | b"idat") {
        return bytes;
    }
    if depth == 0 && !matches!(&kind, b"ftyp" | b"meta") {
        return bytes;
    }
    if depth >= 4 {
        return bytes
            .saturating_add((payload.len() as u64).saturating_mul(METADATA_PAYLOAD_COPIES));
    }
    let children = match &kind {
        b"meta" => payload.get(4..),
        b"iprp" | b"ipco" => Some(payload),
        b"iinf" => payload
            .first()
            .and_then(|version| payload.get(if *version == 0 { 6 } else { 8 }..)),
        b"iref" => payload.get(4..),
        _ => None,
    };
    if let Some(children) = children {
        for child in isobmff::BoxIter::new(children) {
            let Ok(child) = child else { break };
            bytes = bytes.saturating_add(box_metadata_bytes(&child, depth + 1));
            if kind == *b"iref" {
                let mut cursor = if payload.first() == Some(&0) { 2 } else { 4 };
                if let Some(count) = read_field(child.payload, &mut cursor, 2) {
                    bytes = bytes.saturating_add(
                        count.saturating_mul(size_of::<u32>() as u64 * METADATA_TABLE_COPIES),
                    );
                }
            }
        }
    } else {
        bytes =
            bytes.saturating_add((payload.len() as u64).saturating_mul(METADATA_PAYLOAD_COPIES));
    }
    match &kind {
        b"iloc" => bytes.saturating_add(location_table_bytes(payload).unwrap_or(0)),
        b"ipma" => bytes.saturating_add(association_table_bytes(payload).unwrap_or(0)),
        b"hvcC" => bytes.saturating_add(
            hevc_configuration_table_bytes(payload)
                .unwrap_or(0)
                .saturating_mul(METADATA_TABLE_COPIES),
        ),
        _ => bytes,
    }
}

fn metadata_bytes(input: &[u8]) -> u64 {
    let mut bytes = 0u64;
    for parsed in isobmff::BoxIter::new(input) {
        let Ok(parsed) = parsed else { break };
        bytes = bytes.saturating_add(box_metadata_bytes(&parsed, 0));
    }
    bytes
}

#[derive(Default)]
struct ItemStorage {
    payload_bytes: u64,
    property_bytes: u64,
    alpha: bool,
    hevc_config: bool,
    dimensions: Option<(u32, u32)>,
    data_reference_index: Option<u16>,
    hevc: bool,
}

const PAYLOAD_STORAGE_COPIES: u64 = 6;
const PROPERTY_STORAGE_COPIES: u64 = 3;

impl ItemStorage {
    fn bytes(&self) -> u64 {
        self.payload_bytes
            .saturating_mul(PAYLOAD_STORAGE_COPIES)
            .saturating_add(self.property_bytes.saturating_mul(PROPERTY_STORAGE_COPIES))
    }
}

fn item_storage(resolved: &isobmff::ResolvedPrimaryItemGraph<'_>) -> HashMap<u32, ItemStorage> {
    let mut items = HashMap::<u32, ItemStorage>::new();
    for item in &resolved.iloc.items {
        items.entry(item.item_id).or_insert_with(|| ItemStorage {
            payload_bytes: item
                .extents
                .iter()
                .fold(0u64, |bytes, extent| bytes.saturating_add(extent.length)),
            data_reference_index: Some(item.data_reference_index),
            ..Default::default()
        });
    }
    let mut types = HashMap::new();
    for info in &resolved.iinf.entries {
        types.entry(info.item_id).or_insert(info.item_type);
    }
    for (&id, item) in &mut items {
        item.hevc = types
            .get(&id)
            .copied()
            .flatten()
            .is_some_and(|kind| matches!(&kind.as_bytes(), b"hvc1" | b"hev1"));
    }
    let properties: Vec<_> = resolved
        .iprp
        .property_containers
        .iter()
        .flat_map(|container| &container.properties)
        .map(|property| {
            let typed_bytes = match &property.header.box_type.as_bytes() {
                b"hvcC" => hevc_configuration_table_bytes(property.payload).unwrap_or(0),
                b"clap" | b"irot" | b"imir" => {
                    size_of::<isobmff::PrimaryItemTransformProperty>() as u64 * 2
                }
                _ => 0,
            };
            (
                property,
                (property.payload.len() as u64).saturating_add(typed_bytes),
            )
        })
        .collect();
    for associations in &resolved.iprp.associations {
        for entry in &associations.entries {
            let item = items.entry(entry.item_id).or_default();
            for association in &entry.associations {
                if association.property_index == 0 {
                    continue;
                }
                let Some((property, bytes)) =
                    properties.get(usize::from(association.property_index - 1))
                else {
                    continue;
                };
                item.property_bytes = item.property_bytes.saturating_add(*bytes);
                item.alpha |= property_is_alpha_auxiliary_type_property(property);
                item.hevc_config |= property.header.box_type.as_bytes() == HVCC_PROPERTY_TYPE;
                if item.dimensions.is_none() && property.header.box_type.as_bytes() == *b"ispe" {
                    item.dimensions = property
                        .parse_ispe()
                        .ok()
                        .map(|ispe| (ispe.width, ispe.height));
                }
            }
        }
    }
    items
}

fn alpha_storage(
    items: &HashMap<u32, ItemStorage>,
    item_id: u32,
    expected: Option<(u32, u32)>,
) -> Option<u64> {
    let item = items.get(&item_id)?;
    if !item.hevc
        || item.data_reference_index != Some(0)
        || !item.alpha
        || !item.hevc_config
        || expected
            .zip(item.dimensions)
            .is_some_and(|(expected, actual)| expected != actual)
    {
        return None;
    }
    Some(item.bytes())
}

pub(super) fn validate_input(input: &[u8], guardrails: &DecodeOptions) -> Result<(), DecodeError> {
    if guardrails.max_memory_bytes.is_none() {
        return Ok(());
    }
    let metadata = metadata_bytes(input);
    enforce(
        guardrails,
        metadata.saturating_add((input.len() as u64).saturating_mul(INPUT_STORAGE_COPIES)),
    )?;
    let Ok(boxes) = isobmff::parse_boxes(input) else {
        return Ok(());
    };
    let Some(meta_box) = find_first_box_by_type(&boxes, META_BOX_TYPE) else {
        return Ok(());
    };
    let Ok(meta) = meta_box.parse_meta() else {
        return Ok(());
    };
    let Ok(resolved) = meta.resolve_primary_item() else {
        return Ok(());
    };
    if resolved
        .primary_item
        .item_info
        .item_type
        .is_some_and(|kind| matches!(&kind.as_bytes(), b"hvc1" | b"hev1" | b"grid"))
    {
        let items = item_storage(&resolved);
        let item_bytes = |item_id| items.get(&item_id).map_or(0, ItemStorage::bytes);
        let mut retained = item_bytes(resolved.primary_item.item_id);
        if resolved
            .primary_item
            .item_info
            .item_type
            .is_some_and(|kind| kind.as_bytes() == *b"grid")
        {
            for reference in &resolved.primary_item.references {
                if reference.reference_type.as_bytes() == *b"dimg" {
                    for &item_id in &reference.to_item_ids {
                        retained = retained.saturating_add(item_bytes(item_id)).saturating_add(
                            (size_of::<isobmff::HeicGridTileItemData>() as u64
                                + size_of::<u32>() as u64 * 2)
                                .saturating_mul(2),
                        );
                    }
                }
            }
        }
        return enforce(
            guardrails,
            retained
                .saturating_add((input.len() as u64).saturating_mul(INPUT_STORAGE_COPIES))
                .saturating_add(metadata),
        );
    }
    Err(DecodeError::Unsupported(
        "aggregate memory budgeting requires HEVC-backed HEIC".into(),
    ))
}

pub(super) fn enforce(guardrails: &DecodeOptions, bytes: u64) -> Result<(), DecodeError> {
    if let Some(limit) = guardrails.max_memory_bytes
        && bytes > limit
    {
        return Err(DecodeGuardrailError::DecodedByteCountExceeded {
            stage: "aggregate decode",
            actual_bytes: bytes,
            max_decoded_bytes: limit,
        }
        .into());
    }
    Ok(())
}

fn input_bytes(input: &[u8], source: &Option<&mut dyn RandomAccessSource>) -> u64 {
    source
        .as_ref()
        .map_or(input.len() as u64, |source| source.len())
        .saturating_mul(2)
        .saturating_add(input.len() as u64)
}

pub(super) fn validate_exif(
    input: &[u8],
    source: &Option<&mut dyn RandomAccessSource>,
    options: &DecodeOptions,
    retained_bytes: u64,
) -> Result<(), DecodeError> {
    if options.max_memory_bytes.is_none() {
        return Ok(());
    }
    let base = input_bytes(input, source)
        .saturating_add(metadata_bytes(input))
        .saturating_add(retained_bytes);
    enforce(options, base)?;
    let Ok(boxes) = isobmff::parse_boxes(input) else {
        return Ok(());
    };
    let Some(meta_box) = find_first_box_by_type(&boxes, META_BOX_TYPE) else {
        return Ok(());
    };
    let Ok(meta) = meta_box.parse_meta() else {
        return Ok(());
    };
    let Ok(resolved) = meta.resolve_primary_item() else {
        return Ok(());
    };
    let Some(iref) = &resolved.iref else {
        return Ok(());
    };
    let items = item_storage(&resolved);
    let mut infos = HashMap::new();
    for info in &resolved.iinf.entries {
        infos.entry(info.item_id).or_insert(info);
    }
    let mut payloads = 0u64;
    let mut count = 0u64;
    for reference in &iref.references {
        if reference.reference_type.as_bytes() != CDSC_REFERENCE_TYPE
            || !reference
                .to_item_ids
                .contains(&resolved.primary_item.item_id)
            || !infos
                .get(&reference.from_item_id)
                .is_some_and(|info| item_info_is_exif_candidate(info))
        {
            continue;
        }
        let Some(item) = items.get(&reference.from_item_id) else {
            continue;
        };
        if item.data_reference_index != Some(0) {
            continue;
        }
        payloads = payloads.saturating_add(item.payload_bytes);
        count = count.saturating_add(1);
    }
    enforce(
        options,
        base.saturating_add(payloads.saturating_mul(3))
            .saturating_add(count.saturating_mul(size_of::<Vec<u8>>() as u64 * 2)),
    )
}

fn retained_configuration_bytes(hvcc: &isobmff::HevcDecoderConfigurationBox) -> u64 {
    hvcc.nal_arrays.iter().fold(
        hvcc.nal_arrays.capacity() as u64 * size_of::<isobmff::HevcNalArray>() as u64,
        |bytes, array| {
            array.nal_units.iter().fold(
                bytes.saturating_add(
                    array.nal_units.capacity() as u64 * size_of::<Vec<u8>>() as u64,
                ),
                |bytes, unit| bytes.saturating_add(unit.capacity() as u64),
            )
        },
    )
}

pub(super) fn retained_primary_bytes(primary: &isobmff::HeicPrimaryItemDataWithGrid) -> u64 {
    match primary {
        isobmff::HeicPrimaryItemDataWithGrid::Coded(item) => item.payload.capacity() as u64,
        isobmff::HeicPrimaryItemDataWithGrid::Grid(grid) => {
            let bytes = (grid.tiles.capacity() as u64)
                .saturating_mul(size_of::<isobmff::HeicGridTileItemData>() as u64)
                .saturating_add(grid.tile_item_ids.capacity() as u64 * size_of::<u32>() as u64)
                .saturating_add(
                    grid.colr
                        .icc
                        .as_ref()
                        .map_or(0, |icc| icc.profile.capacity() as u64),
                );
            grid.tiles.iter().fold(bytes, |bytes, tile| {
                bytes
                    .saturating_add(tile.payload.capacity() as u64)
                    .saturating_add(retained_configuration_bytes(&tile.hvcc))
                    .saturating_add(
                        tile.transforms.capacity() as u64
                            * size_of::<isobmff::PrimaryItemTransformProperty>() as u64,
                    )
                    .saturating_add(
                        tile.colr
                            .icc
                            .as_ref()
                            .map_or(0, |icc| icc.profile.capacity() as u64),
                    )
            })
        }
    }
}

fn conversion_peak(pixels: u64, output_bytes: u64, icc_bytes: u64) -> u64 {
    output_bytes
        .saturating_mul(2)
        .saturating_add(pixels.saturating_mul(9))
        .saturating_add(icc_bytes.saturating_mul(4))
        .saturating_add(if icc_bytes == 0 { 0 } else { 16 * 1024 * 1024 })
}

fn frame_peak(
    hvcc: &isobmff::HevcDecoderConfigurationBox,
    payload: &[u8],
    guardrails: &DecodeOptions,
) -> Result<(u64, u64, u8), DecodeError> {
    let decoder_bytes = geometry::preflight_hevc(hvcc, payload, None, guardrails)?;
    let mut pixels = 0u64;
    let mut bit_depth = 8;
    walk_hevc_nals_from_hvcc_or_payload(hvcc, payload, |offset, nal| {
        if (LengthPrefixedHevcNalUnit { offset, bytes: nal }).nal_unit_type()
            == Some(NALUnitType::SpsNut)
        {
            let sps = hevc_sps_from_nal(nal, offset)?;
            pixels = pixels.max(
                u64::from(sps.pic_width_in_luma_samples)
                    .saturating_mul(u64::from(sps.pic_height_in_luma_samples)),
            );
            bit_depth = bit_depth.max(sps.bit_depth_y()).max(sps.bit_depth_c());
        }
        Ok(false)
    })?;
    Ok((
        decoder_bytes
            .saturating_add(payload.len() as u64)
            .saturating_add(pixels)
            .saturating_add(64 * 1024),
        pixels,
        bit_depth,
    ))
}

fn auxiliary_peak(
    input: &[u8],
    source: &mut Option<&mut dyn RandomAccessSource>,
    expected: (u32, u32),
    guardrails: &DecodeOptions,
    retained_bytes: u64,
) -> Result<(u64, u8), DecodeError> {
    let Ok(top_level) = isobmff::parse_boxes(input) else {
        return Ok((0, 8));
    };
    let Some(meta_box) = find_first_box_by_type(&top_level, META_BOX_TYPE) else {
        return Ok((0, 8));
    };
    let Ok(meta) = meta_box.parse_meta() else {
        return Ok((0, 8));
    };
    let Ok(resolved) = meta.resolve_primary_item() else {
        return Ok((0, 8));
    };
    let Some(iref) = resolved.iref.as_ref() else {
        return Ok((0, 8));
    };
    let items = item_storage(&resolved);
    let mut peak = 0;
    let mut bit_depth = 8;
    for reference in &iref.references {
        if reference.reference_type.as_bytes() != AUXL_REFERENCE_TYPE
            || !reference
                .to_item_ids
                .contains(&resolved.primary_item.item_id)
        {
            continue;
        }
        let Some(encoded_bytes) = alpha_storage(&items, reference.from_item_id, Some(expected))
        else {
            continue;
        };
        enforce(guardrails, retained_bytes.saturating_add(encoded_bytes))?;
        let Some((hvcc, payload)) = prepare_auxiliary_alpha_hevc_components(
            input,
            source,
            &meta,
            &resolved,
            reference.from_item_id,
            expected,
            guardrails,
        ) else {
            continue;
        };
        let (frame_bytes, _, depth) = frame_peak(&hvcc, &payload, guardrails)?;
        bit_depth = bit_depth.max(depth);
        peak = peak.max(
            frame_bytes.saturating_add(
                (payload.capacity() as u64)
                    .saturating_add(retained_configuration_bytes(&hvcc))
                    .saturating_mul(2),
            ),
        );
    }
    Ok((peak, bit_depth))
}

pub(super) fn heic(
    input: &[u8],
    source: &mut Option<&mut dyn RandomAccessSource>,
    primary: &isobmff::HeicPrimaryItemDataWithGrid,
    guardrails: &DecodeOptions,
    rgb: bool,
    icc_bytes: u64,
) -> Result<(), DecodeError> {
    if guardrails.max_memory_bytes.is_none() {
        return Ok(());
    }
    let retained_payload = retained_primary_bytes(primary);
    let (width, height, decoder_peak, tile_output, bit_depth) = match primary {
        isobmff::HeicPrimaryItemDataWithGrid::Coded(item) => {
            let properties = parse_and_validate_heic_coded_item_preflight(input, item)?;
            let (bytes, _, bit_depth) = frame_peak(&properties.hvcc, &item.payload, guardrails)?;
            (
                properties.ispe.width,
                properties.ispe.height,
                bytes,
                0,
                bit_depth,
            )
        }
        isobmff::HeicPrimaryItemDataWithGrid::Grid(grid) => {
            geometry::preflight_grid(grid, guardrails)?;
            let mut total = 0u64;
            let mut largest = 0;
            let mut tile_output = 0;
            let mut bit_depth = 8;
            for tile in &grid.tiles {
                let (bytes, pixels, depth) = frame_peak(&tile.hvcc, &tile.payload, guardrails)?;
                total = total.saturating_add(bytes);
                largest = largest.max(bytes);
                tile_output = tile_output.max(pixels.saturating_mul(16));
                bit_depth = bit_depth.max(depth);
            }
            let active = total
                .min(GRID_TILE_DECODE_MEMORY_BUDGET.saturating_mul(2))
                .max(largest);
            (
                grid.descriptor.output_width,
                grid.descriptor.output_height,
                active.saturating_add(largest),
                tile_output,
                bit_depth,
            )
        }
    };
    guardrails.enforce_pixel_count(width, height)?;
    let pixels = u64::from(width) * u64::from(height);
    let retained_bytes = input_bytes(input, source)
        .saturating_add(metadata_bytes(input))
        .saturating_add(retained_payload.saturating_mul(2));
    let (alpha_peak, alpha_depth) = if rgb {
        (0, 8)
    } else {
        auxiliary_peak(input, source, (width, height), guardrails, retained_bytes)?
    };
    let bytes_per_pixel = if rgb {
        3
    } else if bit_depth.max(alpha_depth) > 8 {
        8
    } else {
        4
    };
    let output = pixels.saturating_mul(bytes_per_pixel);
    let decode_peak = decoder_peak
        .saturating_add(alpha_peak)
        .saturating_add(output.saturating_mul(2))
        .saturating_add(tile_output);
    enforce(
        guardrails,
        retained_bytes.saturating_add(decode_peak.max(conversion_peak(pixels, output, icc_bytes))),
    )
}

pub(super) fn png(
    guardrails: &DecodeOptions,
    decoded: &DecodedRgbaImage,
    input_bytes: u64,
) -> Result<(), DecodeError> {
    let sample_bytes = match &decoded.pixels {
        DecodedRgbaPixels::U8(_) => 1u64,
        DecodedRgbaPixels::U16(_) => 2,
    };
    let retained_bytes = match &decoded.pixels {
        DecodedRgbaPixels::U8(pixels) => pixels.capacity() as u64,
        DecodedRgbaPixels::U16(pixels) => pixels.capacity() as u64 * 2,
    };
    let endian_copy = match &decoded.pixels {
        DecodedRgbaPixels::U8(_) => 0,
        DecodedRgbaPixels::U16(pixels) => pixels.len() as u64 * 2,
    };
    let row_bytes = u64::from(decoded.width)
        .saturating_mul(4)
        .saturating_mul(sample_bytes)
        .saturating_add(1);
    let raw_stream = u64::from(decoded.height).saturating_mul(row_bytes);
    enforce(
        guardrails,
        retained_bytes
            .saturating_add(endian_copy)
            .saturating_add(raw_stream.saturating_mul(3))
            .saturating_add(row_bytes.saturating_mul(2))
            .saturating_add(decoded.icc_profile.as_ref().map_or(0, |profile| {
                (profile.capacity() as u64).saturating_add((profile.len() as u64).saturating_mul(4))
            }))
            .saturating_add(input_bytes)
            .saturating_add(PNG_ENCODER_BYTES),
    )
}
