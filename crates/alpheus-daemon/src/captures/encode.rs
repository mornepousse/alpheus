//! The PNG writer of a capture: RGBA bytes in, PNG bytes out.
//!
//! The `png` crate with its fast compression: a capture is encoded once, on the short-lived
//! `alpheus-capture` thread, so the speed matters more than the last percent of size. Pure: it
//! reads no file and no clock.
//!
//! Never:
//! - write a file (the caller saves or copies the bytes);
//! - keep a buffer after it returns.
//!
//! See: docs/features/captures.md (when it lands), docs/decisions.md.

use png::{BitDepth, ColorType, Compression, Encoder};

/// Why `encode_png` gave nothing: the bytes do not match the size, or the encoder refused.
#[derive(Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// `rgba` is not `w * h * 4` bytes, or `w` or `h` is zero.
    Size,
    /// The encoder failed.
    Encoder,
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EncodeError::Size => "image size",
            EncodeError::Encoder => "png encoder",
        })
    }
}

impl std::error::Error for EncodeError {}

/// `rgba` (row-major, four bytes a pixel) as a PNG of `w` by `h`.
pub fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, EncodeError> {
    let expected = (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4));
    if w == 0 || h == 0 || expected != Some(rgba.len()) {
        return Err(EncodeError::Size);
    }
    // Compressed data is rarely above the raw size: one allocation for the common case.
    let mut out = Vec::with_capacity(rgba.len() / 2 + 1024);
    let mut encoder = Encoder::new(&mut out, w, h);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    encoder.set_compression(Compression::Fast);
    let mut writer = encoder.write_header().map_err(|_| EncodeError::Encoder)?;
    writer.write_image_data(rgba).map_err(|_| EncodeError::Encoder)?;
    writer.finish().map_err(|_| EncodeError::Encoder)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes with the same crate, to RGBA.
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, ColorType::Rgba);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn encode_png_round_trips_a_small_image() {
        let (w, h) = (3, 2);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 + 3) as u8).collect();
        let png = encode_png(&rgba, w, h).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(decode(&png), (w, h, rgba));
    }

    #[test]
    fn a_wrong_size_is_refused() {
        assert_eq!(encode_png(&[0; 15], 2, 2), Err(EncodeError::Size));
        assert_eq!(encode_png(&[], 0, 0), Err(EncodeError::Size));
        assert_eq!(encode_png(&[0; 16], 2, 2).map(|_| ()), Ok(()));
    }
}
