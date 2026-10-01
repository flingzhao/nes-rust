const DEBUG_COLORS: [[u8; 3]; 4] = [[0, 0, 0], [85, 85, 85], [170, 170, 170], [255, 255, 255]];

use crate::cartridge::Cartridge;
use crate::ppu::Ppu;

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

pub fn build_background_indices(
    ppu: &Ppu,
    cartridge: &Cartridge,
    nametable_base: u16,
    pattern_base: u16,
) -> Result<Vec<u8>, String> {
    // 1. 检查两个基地址。
    // 2. 分配 256 * 240 字节。
    // 3. 遍历 30 行、32 列图块。
    // 4. 读取图块编号和对应属性字节。
    // 5. 遍历图块的 8 行，调用 decode_tile_row。
    // 6. 根据像素编号、子调色板，读取最终颜色编号。
    // 7. 写入 framebuffer[y * 256 + x]。
    match nametable_base {
        0x2000 | 0x2400 | 0x2800 | 0x2C00 => {}
        _ => {
            return Err(format!(
                "nametable_base only accept 0x2000/0x2400/0x2800/0x2C00, got  0x{:04X}",
                nametable_base
            ));
        }
    };
    match pattern_base {
        0x0000 | 0x1000 => {}
        _ => {
            return Err(format!(
                "pattern_base only accept 0x0000/0x1000, got  0x{:04X}",
                pattern_base
            ));
        }
    };

    let mut framebuffer = vec![0u8; 256 * 240];

    for y in 0..240 {
        let tile_y = y / 8;
        for x in 0..256 {
            let tile_x = x / 8;
            let addr = nametable_base + (tile_y * 32 + tile_x) as u16;
            let tile_id = ppu.read(addr, cartridge)?;

            let pattern_addr = pattern_base + u16::from(tile_id) * 16;

            let row_in_tile = (y % 8) as u16;

            let low_addr = pattern_addr + row_in_tile;
            let high_addr = pattern_addr + row_in_tile + 8;

            let low = ppu.read(low_addr, cartridge)?;
            let high = ppu.read(high_addr, cartridge)?;
            let pixels = decode_tile_row(low, high);
            let pixel = pixels[x % 8];

            let attr_y = tile_y / 4;
            let attr_x = tile_x / 4;
            let attribute_offset = attr_y * 8 + attr_x;
            let attribute_addr = nametable_base + 0x03C0 + attribute_offset as u16;
            let attribute = ppu.read(attribute_addr, cartridge)?;

            let right = tile_x % 4 >= 2;
            let bottom = tile_y % 4 >= 2;

            let shift = match (bottom, right) {
                (false, false) => 0,
                (false, true) => 2,
                (true, false) => 4,
                (true, true) => 6,
            };

            let palette_id = (attribute >> shift) & 0x03;

            let addr = if pixel == 0 {
                0x3F00
            } else {
                0x3F00 + u16::from(palette_id) * 4 + u16::from(pixel)
            };

            let color_index = ppu.read(addr, cartridge)?;

            framebuffer[y * 256 + x] = color_index;
        }
    }

    Ok(framebuffer)
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

#[cfg(test)]
mod background_tests {
    use super::*;

    fn fixture() -> (Ppu, Cartridge) {
        // iNES：16 KiB PRG、CHR RAM、垂直镜像。
        let mut rom = vec![0u8; 16 + 16 * 1024];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;
        rom[6] = 1;

        let mut cartridge = Cartridge::try_from(rom.as_slice()).unwrap();
        let mut ppu = Ppu::new();

        for row in 0..8u16 {
            // 表 0 的图块 1：所有像素编号都是 1。
            cartridge.ppu_write(0x0010 + row, 0xFF).unwrap();

            // 表 1 的图块 1：所有像素编号都是 2。
            cartridge.ppu_write(0x1018 + row, 0xFF).unwrap();
        }

        // 第一张名称表全部使用图块 1。
        for offset in 0..960u16 {
            ppu.write(0x2000 + offset, 1, &mut cartridge).unwrap();
        }

        // 每个属性区域：左上 0、右上 1、左下 2、右下 3。
        for offset in 0..64u16 {
            ppu.write(0x23C0 + offset, 0xE4, &mut cartridge).unwrap();
        }

        // 为便于判断，颜色编号等于调色板内偏移。
        for offset in 1..16u16 {
            ppu.write(0x3F00 + offset, offset as u8, &mut cartridge)
                .unwrap();
        }

        ppu.write(0x3F00, 0x0F, &mut cartridge).unwrap();

        (ppu, cartridge)
    }

    #[test]
    fn background_uses_all_attribute_quadrants() {
        let (ppu, cartridge) = fixture();
        let pixels = build_background_indices(&ppu, &cartridge, 0x2000, 0).unwrap();

        assert_eq!(pixels.len(), 256 * 240);

        // 检查整幅图片，包含属性表末行和右下角。
        for y in 0..240usize {
            for x in 0..256usize {
                let palette = ((y % 32) / 16) * 2 + ((x % 32) / 16);
                let expected = (palette * 4 + 1) as u8;

                assert_eq!(pixels[y * 256 + x], expected, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn zero_pixel_uses_universal_backdrop() {
        let (mut ppu, mut cartridge) = fixture();

        // 图块 0 全零，放在使用子调色板 1 的区域。
        ppu.write(0x2002, 0, &mut cartridge).unwrap();

        let pixels = build_background_indices(&ppu, &cartridge, 0x2000, 0).unwrap();

        for y in 0..8usize {
            for x in 16..24usize {
                // 必须读取 3F00 的 0F，而不是 3F04 的 04。
                assert_eq!(pixels[y * 256 + x], 0x0F);
            }
        }

        // 相邻图块仍正常使用子调色板 1 的颜色 1。
        assert_eq!(pixels[24], 5);
    }

    #[test]
    fn background_selects_pattern_table() {
        let (ppu, cartridge) = fixture();

        let first = build_background_indices(&ppu, &cartridge, 0x2000, 0).unwrap();
        let second = build_background_indices(&ppu, &cartridge, 0x2000, 0x1000).unwrap();

        assert_eq!(first[0], 1);
        assert_eq!(second[0], 2);

        // 子调色板 1：4 + 像素编号 2。
        assert_eq!(second[16], 6);
    }

    #[test]
    fn background_selects_nametable() {
        let (mut ppu, mut cartridge) = fixture();

        // 垂直镜像下，2400 与 2000 是不同物理名称表。
        // 2400 默认全部使用全零图块。
        let blank = build_background_indices(&ppu, &cartridge, 0x2400, 0).unwrap();
        assert!(blank.iter().all(|&value| value == 0x0F));

        ppu.write(0x2400, 1, &mut cartridge).unwrap();

        let pixels = build_background_indices(&ppu, &cartridge, 0x2400, 0).unwrap();

        assert_eq!(pixels[0], 1);
        assert_eq!(pixels[7 * 256 + 7], 1);
        assert_eq!(pixels[8], 0x0F);
        assert_eq!(pixels[8 * 256], 0x0F);
    }

    #[test]
    fn background_rejects_invalid_bases() {
        let (ppu, cartridge) = fixture();

        assert!(build_background_indices(&ppu, &cartridge, 0x2001, 0).is_err());
        assert!(build_background_indices(&ppu, &cartridge, 0x3000, 0).is_err());
        assert!(build_background_indices(&ppu, &cartridge, 0x2000, 0x0800).is_err());
    }

    #[test]
    fn background_reads_correct_row_and_column_inside_tile() {
        let (ppu, mut cartridge) = fixture();

        // 清空图案表 0 的图块 1。
        for offset in 0..16u16 {
            cartridge.ppu_write(0x0010 + offset, 0).unwrap();
        }

        // 第 1 行、第 0 列：像素编号 1。
        cartridge.ppu_write(0x0010 + 1, 0x80).unwrap();

        // 第 2 行、第 1 列：像素编号 2。
        cartridge.ppu_write(0x0010 + 8 + 2, 0x40).unwrap();

        let pixels = build_background_indices(&ppu, &cartridge, 0x2000, 0).unwrap();

        assert_eq!(pixels.len(), 256 * 240);

        // fixture 中左上角使用子调色板 0：
        // 像素 1 → 颜色编号 1
        // 像素 2 → 颜色编号 2
        // 像素 0 → 公共背景色 0x0F
        for y in 0..8usize {
            for x in 0..8usize {
                let expected = match (x, y) {
                    (0, 1) => 1,
                    (1, 2) => 2,
                    _ => 0x0F,
                };

                assert_eq!(pixels[y * 256 + x], expected, "wrong pixel at ({x}, {y})");
            }
        }

        // 相邻图块同样使用图块 1，检查图块内坐标重新从 0 开始。
        assert_eq!(pixels[256 + 8], 1);
        assert_eq!(pixels[2 * 256 + 9], 2);
        assert_eq!(pixels[8], 0x0F);
    }
}
