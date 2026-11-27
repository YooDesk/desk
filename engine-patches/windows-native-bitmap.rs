// Read opaque Windows clipboard bitmaps through GDI, not synthesized DIB offsets.
use super::*;

pub(super) fn read(bitmap: HBITMAP, width: i32, height: i32) -> Result<ImageData<'static>, Error> {
    let height = height
        .checked_abs()
        .ok_or_else(|| Error::unknown("Invalid bitmap height"))?;
    let size = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    let size = match size {
        Some(size) if width > 0 && height > 0 && size <= 16 << 20 => size,
        _ => return Err(Error::unknown("Invalid clipboard bitmap dimensions")),
    };
    unsafe {
        let dc = get_screen_device_context()?;
        let _dc = ScopeGuard::new(|| {
            windows_sys::Win32::Graphics::Gdi::ReleaseDC(0, dc);
        });
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB as _;
        let mut pixels = vec![0; size];
        let lines = convert_bitmap_to_rgb(
            dc,
            bitmap,
            height as _,
            pixels.as_mut_ptr().cast(),
            &mut info,
        )?;
        if lines != height as _ {
            return Err(Error::unknown("Incomplete clipboard bitmap rows"));
        }
        let mut pixels = win_to_rgba(&mut pixels);
        set_alpha_opaque(&mut pixels);
        // The clipboard owns this HBITMAP. Never delete the borrowed handle.
        Ok(ImageData::rgba(width as _, height as _, Cow::Owned(pixels)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_alpha_dib_keeps_its_pixels_and_transparency() {
        let mut header: BITMAPV5HEADER = unsafe { std::mem::zeroed() };
        header.bV5Size = size_of::<BITMAPV5HEADER>() as _;
        header.bV5Width = 2;
        header.bV5Height = 1;
        header.bV5Planes = 1;
        header.bV5BitCount = 32;
        header.bV5Compression = BI_BITFIELDS;
        header.bV5SizeImage = 8;
        header.bV5RedMask = 0x00ff0000;
        header.bV5GreenMask = 0x0000ff00;
        header.bV5BlueMask = 0x000000ff;
        header.bV5AlphaMask = 0xff000000;
        let mut data = unsafe {
            std::slice::from_raw_parts(
                (&header as *const BITMAPV5HEADER).cast::<u8>(),
                size_of::<BITMAPV5HEADER>(),
            )
        }
        .to_vec();
        data.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]);
        let ImageData::Rgba(image) = super::super::read_clipboard_dibv5(&data).unwrap() else {
            panic!("expected RGBA");
        };
        assert_eq!(image.bytes.as_ref(), &[255, 0, 0, 0, 0, 255, 0, 0]);
    }

    #[test]
    fn native_bitmap_rows_colors_and_borrowed_handle_survive_reads() {
        for width in [2, 3] {
            let expected: Vec<u8> = (0..width * 2)
                .flat_map(|i| [20 + i * 10, 40 + i * 10, 60 + i * 10, 255])
                .collect();
            let mut bgra = expected.clone();
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 0;
            }
            unsafe {
                let dc = get_screen_device_context().unwrap();
                let _dc = ScopeGuard::new(|| {
                    windows_sys::Win32::Graphics::Gdi::ReleaseDC(0, dc);
                });
                let mut header: BITMAPV5HEADER = std::mem::zeroed();
                header.bV5Size = size_of::<BITMAPV5HEADER>() as _;
                header.bV5Width = width as _;
                header.bV5Height = -2;
                header.bV5Planes = 1;
                header.bV5BitCount = 32;
                header.bV5Compression = BI_RGB;
                let bitmap = create_bitmap_from_dib(dc, &header, bgra.as_ptr().cast()).unwrap();
                let _bitmap = ScopeGuard::new(|| {
                    DeleteObject(bitmap);
                });
                for _ in 0..2 {
                    let image = read(bitmap, width as _, 2).unwrap();
                    let ImageData::Rgba(image) = image else {
                        panic!("expected RGBA");
                    };
                    assert_eq!(image.width, width as usize);
                    assert_eq!(image.height, 2);
                    assert_eq!(image.bytes.as_ref(), expected);
                }
            }
        }
    }

    #[test]
    fn invalid_or_excessive_dimensions_fail_before_accessing_gdi() {
        for (width, height) in [
            (0, 2),
            (-1, 2),
            (2, 0),
            (2, i32::MIN),
            (i32::MAX, i32::MAX),
            (4096, 4096),
        ] {
            assert!(read(0, width, height).is_err());
        }
    }
}
