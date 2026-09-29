const DEBUG_COLORS: [[u8; 3]; 4] = [[0, 0, 0], [85, 85, 85], [170, 170, 170], [255, 255, 255]];

use crate::cartridge::Cartridge;

pub const PATTERN_WIDTH: usize = 256;
pub const PATTERN_HEIGHT: usize = 128;

pub fn decode_tile_row(low: u8, high: u8) -> [u8; 8] {
    let mut result = [0u8; 8];

    for i in 0..8 {
        let shift = 7 - i;
        result[i] = ((low >> shift) & 0x01) | ((high >> shift) & 0x01) << 1;
    }

    result
}

/// 返回两张图案表并排显示的 RGB24 数据。
pub fn build_pattern_tables(cartridge: &Cartridge) -> Result<Vec<u8>, String> {
    let mut buffer = vec![0u8; 256 * 128 * 3];

    for table in 0..2_usize {
        for y_count in 0..16 {
            for x_count in 0..16 {
                let addr = table * 0x1000 + 16 * (y_count * 16 + x_count);
                for yoffset in 0..8 {
                    let addr = addr + yoffset;
                    let low = cartridge.ppu_read(addr as u16)?;
                    let addr = addr + 8;
                    let high = cartridge.ppu_read(addr as u16)?;
                    let indices = decode_tile_row(low, high);
                    let y = usize::from(y_count * 8 + yoffset);
                    for xoffset in 0..8 {
                        let x = usize::from(x_count * 8 + table * 128 + xoffset);
                        let start = (x + y * 256) * 3;
                        for j in 0..3 {
                            buffer[start + j] = DEBUG_COLORS[usize::from(indices[xoffset])][j];
                        }
                    }
                }
            }
        }
    }

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_row_combines_planes_from_left_to_right() {
        assert_eq!(
            decode_tile_row(0b0101_0101, 0b0011_0011),
            [0, 1, 2, 3, 0, 1, 2, 3]
        );

        assert_eq!(
            decode_tile_row(0b1000_0000, 0b0000_0001),
            [1, 0, 0, 0, 0, 0, 0, 2]
        );

        assert_eq!(decode_tile_row(0, 0), [0; 8]);
        assert_eq!(decode_tile_row(0xFF, 0xFF), [3; 8]);
    }

    #[test]
    fn pattern_tables_place_tiles_and_planes_correctly() {
        let mut rom = vec![0u8; 16 + 16384 + 8192];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;
        rom[5] = 1;

        let chr = &mut rom[16 + 16384..];

        // 左表第 0 块，第一行最左像素：编号 1。
        chr[0] = 0x80;

        // 左表第 16 块：下一行图块的首块，编号 2。
        chr[16 * 16 + 8] = 0x80;

        // 右表第 0 块，第一行最左像素：编号 3。
        chr[0x1000] = 0x80;
        chr[0x1000 + 8] = 0x80;

        // 右表最后一个图块，最后一行最右像素：编号 3。
        let last_tile = 0x1000 + 255 * 16;
        chr[last_tile + 7] = 0x01;
        chr[last_tile + 15] = 0x01;

        let cartridge = Cartridge::try_from(rom.as_slice()).unwrap();
        let actual = build_pattern_tables(&cartridge).unwrap();

        let mut expected = vec![0u8; 256 * 128 * 3];

        for (x, y, gray) in [
            (0usize, 0usize, 85u8),
            (0, 8, 170),
            (128, 0, 255),
            (255, 127, 255),
        ] {
            let index = (y * 256 + x) * 3;
            expected[index..index + 3].copy_from_slice(&[gray; 3]);
        }

        assert_eq!(actual.len(), expected.len());

        // 避免失败时打印整张图的近十万个字节。
        for (index, (&got, &want)) in actual.iter().zip(expected.iter()).enumerate() {
            assert_eq!(got, want, "RGB byte mismatch at {index}");
        }
    }
}
