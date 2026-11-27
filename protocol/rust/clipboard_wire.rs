//! Bounded clipboard frame: magic, kind, width, height, then unencoded bytes.
pub const MAX_BYTES: usize = 16 << 20;
pub const HEADER: usize = 16;
pub const MAGIC: &[u8; 4] = b"YDC1";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Content<'a> {
    Empty,
    Text(&'a str),
    Image {
        width: u32,
        height: u32,
        bytes: &'a [u8],
    },
}

pub fn decode(frame: &[u8]) -> Result<Content<'_>, &'static str> {
    if frame.len() < HEADER || frame.len() > HEADER + MAX_BYTES || &frame[..4] != MAGIC {
        return Err("invalid clipboard frame");
    }
    let field = |offset| u32::from_le_bytes(frame[offset..offset + 4].try_into().unwrap());
    let (kind, width, height) = (field(4), field(8), field(12));
    let bytes = &frame[HEADER..];
    match kind {
        0 if width == 0 && height == 0 && bytes.is_empty() => Ok(Content::Empty),
        1 if width == 0 && height == 0 => std::str::from_utf8(bytes)
            .map(Content::Text)
            .map_err(|_| "invalid clipboard UTF-8"),
        2 if width > 0
            && height > 0
            && width <= 8192
            && height <= 8192
            && (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                == Some(bytes.len()) =>
        {
            Ok(Content::Image {
                width,
                height,
                bytes,
            })
        }
        _ => Err("invalid clipboard dimensions or payload"),
    }
}

pub fn encode(content: Content<'_>) -> Result<Vec<u8>, &'static str> {
    let (kind, width, height, bytes) = match content {
        Content::Empty => (0u32, 0u32, 0u32, &[][..]),
        Content::Text(text) => (1, 0, 0, text.as_bytes()),
        Content::Image {
            width,
            height,
            bytes,
        } => (2, width, height, bytes),
    };
    if bytes.len() > MAX_BYTES {
        return Err("clipboard payload too large");
    }
    let mut frame = Vec::with_capacity(HEADER + bytes.len());
    frame.extend_from_slice(MAGIC);
    for field in [kind, width, height] {
        frame.extend_from_slice(&field.to_le_bytes());
    }
    frame.extend_from_slice(bytes);
    decode(&frame)?;
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_pixels_and_utf8_round_trip_and_malformed_frames_fail() {
        for content in [
            Content::Empty,
            Content::Text(""),
            Content::Text("你好\0"),
            Content::Image {
                width: 1,
                height: 1,
                bytes: &[0, 128, 255, 0],
            },
        ] {
            let frame = encode(content).unwrap();
            assert_eq!(decode(&frame).unwrap(), content);
        }
        assert!(decode(b"YDC1").is_err());
        assert!(encode(Content::Image {
            width: 2,
            height: 1,
            bytes: &[0; 4]
        })
        .is_err());
        let mut frame = encode(Content::Text("x")).unwrap();
        frame[16] = 255;
        assert!(decode(&frame).is_err());
        frame[4] = 3;
        assert!(decode(&frame).is_err());
    }
}
