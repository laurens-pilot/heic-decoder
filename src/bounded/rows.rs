use super::{Result, codec};
use crate::heic_decoder::hevc::DecodedFrame;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub crop: [u32; 4],
    pub full_range: bool,
    pub matrix: u8,
    pub primaries: u8,
    pub transfer: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Band<'a> {
    pub top: u32,
    pub rows: u32,
    pub width: u32,
    pub luma: &'a [u16],
    pub luma_stride: usize,
    pub cb: &'a [u16],
    pub cr: &'a [u16],
    pub chroma_stride: usize,
}

pub fn info(config: &[u8], payload: &[u8]) -> Result<Info> {
    let geometry = codec::prepare(config, payload)?.geometry;
    Ok(Info {
        width: geometry.width,
        height: geometry.height,
        coded_width: geometry.coded_width,
        coded_height: geometry.coded_height,
        crop: geometry.crop,
        full_range: geometry.full_range,
        matrix: geometry.matrix,
        primaries: geometry.primaries,
        transfer: geometry.transfer,
    })
}

pub fn decode_rows(config: &[u8], payload: &[u8], mut emit: impl FnMut(Band<'_>)) -> Result<()> {
    codec::prepare(config, payload)?.decode_incremental(|top, frame| {
        emit(band(top, frame));
        Ok(())
    })?;
    Ok(())
}

fn band(top: u32, frame: &DecodedFrame) -> Band<'_> {
    let luma_stride = frame.y_stride();
    let chroma_stride = frame.c_stride();
    let luma_len = frame.height as usize * luma_stride;
    let chroma_len = frame.height.div_ceil(2) as usize * chroma_stride;
    Band {
        top,
        rows: frame.height,
        width: frame.width,
        luma: &frame.y_plane[..luma_len],
        luma_stride,
        cb: &frame.cb_plane[..chroma_len],
        cr: &frame.cr_plane[..chroma_len],
        chroma_stride,
    }
}
