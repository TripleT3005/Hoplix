// Constants

const ICON_FILE: &[u8] = include_bytes!("../../../assets/icons/icon.ico");
const DIRECTORY_ENTRY_SIZE: usize = 16;
const BITMAP_HEADER_SIZE: usize = 40;

// Window icon

pub fn window_icon(size: u8) -> Option<iced::window::Icon> {
    let pixels = icon_pixels(ICON_FILE, size)?;
    iced::window::icon::from_rgba(pixels, u32::from(size), u32::from(size)).ok()
}

// Decoding

fn icon_pixels(file: &[u8], size: u8) -> Option<Vec<u8>> {
    let count = usize::from(u16::from_le_bytes([*file.get(4)?, *file.get(5)?]));
    let image = (0..count).find_map(|index| {
        let entry =
            file.get(6 + index * DIRECTORY_ENTRY_SIZE..6 + (index + 1) * DIRECTORY_ENTRY_SIZE)?;
        if entry[0] != size {
            return None;
        }
        let length = usize::try_from(u32::from_le_bytes(entry[8..12].try_into().ok()?)).ok()?;
        let offset = usize::try_from(u32::from_le_bytes(entry[12..16].try_into().ok()?)).ok()?;
        file.get(offset..offset.checked_add(length)?)
    })?;
    decode_bitmap(image, usize::from(size))
}

fn decode_bitmap(image: &[u8], size: usize) -> Option<Vec<u8>> {
    let bits = u16::from_le_bytes([*image.get(14)?, *image.get(15)?]);
    if bits != 32 {
        return None;
    }
    let pixels = image.get(BITMAP_HEADER_SIZE..BITMAP_HEADER_SIZE + size * size * 4)?;
    let mut rgba = Vec::with_capacity(size * size * 4);
    for row in pixels.chunks_exact(size * 4).rev() {
        for [blue, green, red, alpha] in row.as_chunks::<4>().0 {
            rgba.extend_from_slice(&[*red, *green, *blue, *alpha]);
        }
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes_at_window_sizes() {
        for size in [16u8, 32, 64] {
            let pixels = icon_pixels(ICON_FILE, size).expect("icon size missing");
            assert_eq!(pixels.len(), usize::from(size) * usize::from(size) * 4);
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] == 255)
            );
        }
    }

    #[test]
    fn rows_are_flipped_and_channels_swapped() {
        let mut image = vec![0; BITMAP_HEADER_SIZE];
        image[14] = 32;
        image.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        image.extend_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16]);
        let pixels = decode_bitmap(&image, 2).expect("decoding failed");
        assert_eq!(&pixels[..8], &[11, 10, 9, 12, 15, 14, 13, 16]);
        assert_eq!(&pixels[8..], &[3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn missing_sizes_and_other_formats_are_rejected() {
        assert!(icon_pixels(ICON_FILE, 17).is_none());
        let mut image = vec![0; BITMAP_HEADER_SIZE + 16];
        image[14] = 24;
        assert!(decode_bitmap(&image, 2).is_none());
    }
}
