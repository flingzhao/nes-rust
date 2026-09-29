use crate::cartridge::{Cartridge, Mirroring};

// Horizontal mirroring:
// 0x0000-0x03FF => 0x0000-0x03FF
// 0x0400-0x07FF => 0x0000-0x03FF
// 0x0800-0x0BFF => 0x0400-0x07FF
// 0x0C00-0x0FFF => 0x0400-0x07FF
// Vertical mirroring
// 0x0000-0x03FF => 0x0000-0x03FF
// 0x0400-0x07FF => 0x0400-0x07FF
// 0x0800-0x0BFF => 0x0000-0x03FF
// 0x0C00-0x0FFF => 0x0400-0x07FF
fn nametable_index(addr: u16, mirroring: &Mirroring) -> Result<usize, String> {
    let relative = usize::from(addr & 0xFFF);
    let table = relative / 0x0400;
    let offset = relative % 0x0400;

    let physical_table = match mirroring {
        Mirroring::Horizontal => table / 2,
        Mirroring::Vertical => table % 2,
        Mirroring::FourScreen => {
            return Err("FourScreen has not been implemented".to_string());
        }
    };
    Ok(physical_table * 0x400 + offset)
}

fn palette_index(addr: u16) -> usize {
    let index = usize::from(addr & 0x1F);

    match index {
        0x10 | 0x14 | 0x18 | 0x1C => index - 0x10,
        _ => index,
    }
}

pub struct Ppu {
    dot: u16,
    scanline: u16,
    frame: u64,
    nametable_ram: [u8; 2048],
    palette_ram: [u8; 32],
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            dot: 0,
            scanline: 0,
            frame: 0,
            nametable_ram: [0u8; 2048],
            palette_ram: [0u8; 32],
        }
    }

    /// 推进一个 PPU 周期。
    pub fn clock(&mut self) {
        self.dot += 1;
        if self.dot == 341_u16 {
            self.dot = 0;
            self.scanline += 1;
        }

        if self.scanline == 262 {
            self.scanline = 0;
            self.frame += 1;
        }
    }

    pub fn dot(&self) -> u16 {
        self.dot
    }

    pub fn scanline(&self) -> u16 {
        self.scanline
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn read(&self, addr: u16, cartridge: &Cartridge) -> Result<u8, String> {
        let addr = addr & 0x3FFF;

        match addr {
            0x0000..=0x1FFF => cartridge.ppu_read(addr),
            0x2000..=0x3EFF => {
                // 名称表逻辑
                let index = nametable_index(addr, cartridge.mirroring())?;
                Ok(self.nametable_ram[index])
            }
            0x3F00..=0x3FFF => {
                // 调色板逻辑
                let index = palette_index(addr);
                Ok(self.palette_ram[index])
            }
            _ => unreachable!("address wa masked to 14 bits"),
        }
    }

    pub fn write(&mut self, addr: u16, value: u8, cartridge: &mut Cartridge) -> Result<(), String> {
        let addr = addr & 0x3FFF;

        match addr {
            0x0000..=0x1FFF => cartridge.ppu_write(addr, value)?,
            0x2000..=0x3EFF => {
                // 名称表逻辑
                let index = nametable_index(addr, cartridge.mirroring())?;
                self.nametable_ram[index] = value;
            }
            0x3F00..=0x3FFF => {
                // 调色板逻辑
                let index = palette_index(addr);
                self.palette_ram[index] = value & 0x3F;
            }
            _ => unreachable!("address wa masked to 14 bits"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppu_timing_advances_to_next_scanline() {
        let mut ppu = Ppu::new();

        for _ in 0..340 {
            ppu.clock();
        }

        assert_eq!(ppu.dot(), 340);
        assert_eq!(ppu.scanline(), 0);
        assert_eq!(ppu.frame(), 0);

        ppu.clock();

        assert_eq!(ppu.dot(), 0);
        assert_eq!(ppu.scanline(), 1);
        assert_eq!(ppu.frame(), 0);
    }

    #[test]
    fn ppu_timing_wraps_frames_without_rendering() {
        let mut ppu = Ppu::new();

        for _ in 0..(341 * 262 - 1) {
            ppu.clock();
        }

        assert_eq!(ppu.dot(), 340);
        assert_eq!(ppu.scanline(), 261);
        assert_eq!(ppu.frame(), 0);

        ppu.clock();

        assert_eq!(ppu.dot(), 0);
        assert_eq!(ppu.scanline(), 0);
        assert_eq!(ppu.frame(), 1);

        // 未开启渲染，第二帧同样是完整的 89342 个周期。
        for _ in 0..(341 * 262) {
            ppu.clock();
        }

        assert_eq!(ppu.dot(), 0);
        assert_eq!(ppu.scanline(), 0);
        assert_eq!(ppu.frame(), 2);
    }

    use crate::cartridge::Cartridge;

    fn ppu_test_cartridge(vertical: bool, chr_rom: bool) -> Cartridge {
        let chr_size = if chr_rom { 8192 } else { 0 };
        let mut rom = vec![0u8; 16 + 16384 + chr_size];

        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;
        rom[5] = u8::from(chr_rom);
        rom[6] = u8::from(vertical);

        if chr_rom {
            let start = 16 + 16384;
            rom[start] = 0x12;
            rom[start + 8191] = 0x34;
        }

        Cartridge::try_from(rom.as_slice()).unwrap()
    }

    #[test]
    fn ppu_memory_chr_ram_is_writable() {
        let mut cart = ppu_test_cartridge(false, false);
        let mut ppu = Ppu::new();

        for (addr, value) in [(0x0000, 0x12), (0x1FFF, 0xAB)] {
            ppu.write(addr, value, &mut cart).unwrap();
            assert_eq!(ppu.read(addr, &cart).unwrap(), value);
        }

        // PPU 地址入口只保留低 14 位。
        assert_eq!(ppu.read(0x4000, &cart).unwrap(), 0x12);
    }

    #[test]
    fn ppu_memory_chr_rom_ignores_writes() {
        let mut cart = ppu_test_cartridge(false, true);
        let mut ppu = Ppu::new();

        ppu.write(0x0000, 0xFF, &mut cart).unwrap();
        ppu.write(0x1FFF, 0xFF, &mut cart).unwrap();

        assert_eq!(ppu.read(0x0000, &cart).unwrap(), 0x12);
        assert_eq!(ppu.read(0x1FFF, &cart).unwrap(), 0x34);

        // 卡带 CHR 接口本身拒绝非 CHR 地址。
        assert!(cart.ppu_read(0x2000).is_err());
        assert!(cart.ppu_write(0x2000, 1).is_err());
    }

    #[test]
    fn ppu_memory_nametable_mirroring() {
        for vertical in [false, true] {
            let mut cart = ppu_test_cartridge(vertical, false);
            let mut ppu = Ppu::new();

            let pairs = if vertical {
                [(0x2000u16, 0x2800u16), (0x2400, 0x2C00)]
            } else {
                [(0x2000u16, 0x2400u16), (0x2800, 0x2C00)]
            };

            // 首字节、图块编号区域末尾、属性表起点、整表末尾。
            for offset in [0u16, 0x03BF, 0x03C0, 0x03FF] {
                let (a, a_alias) = pairs[0];
                let (b, b_alias) = pairs[1];

                ppu.write(a + offset, 0x11, &mut cart).unwrap();
                ppu.write(b + offset, 0x22, &mut cart).unwrap();

                assert_eq!(ppu.read(a_alias + offset, &cart).unwrap(), 0x11);
                assert_eq!(ppu.read(b_alias + offset, &cart).unwrap(), 0x22);

                // 镜像必须双向共享同一份存储。
                ppu.write(a_alias + offset, 0x33, &mut cart).unwrap();
                assert_eq!(ppu.read(a + offset, &cart).unwrap(), 0x33);
                assert_eq!(ppu.read(b + offset, &cart).unwrap(), 0x22);
            }
        }
    }

    #[test]
    fn ppu_memory_3000_region_mirrors_nametables() {
        let mut cart = ppu_test_cartridge(true, false);
        let mut ppu = Ppu::new();

        ppu.write(0x2000, 0x12, &mut cart).unwrap();
        assert_eq!(ppu.read(0x3000, &cart).unwrap(), 0x12);

        ppu.write(0x3EFF, 0x34, &mut cart).unwrap();
        assert_eq!(ppu.read(0x2EFF, &cart).unwrap(), 0x34);

        // $6000 的低 14 位为 $2000。
        ppu.write(0x6000, 0x56, &mut cart).unwrap();
        assert_eq!(ppu.read(0x2000, &cart).unwrap(), 0x56);
    }

    #[test]
    fn ppu_memory_palette_aliases_and_masks_values() {
        let mut cart = ppu_test_cartridge(false, false);
        let mut ppu = Ppu::new();

        for (base, alias, value) in [
            (0x3F00u16, 0x3F10u16, 1u8),
            (0x3F04, 0x3F14, 2),
            (0x3F08, 0x3F18, 3),
            (0x3F0C, 0x3F1C, 4),
        ] {
            ppu.write(alias, value, &mut cart).unwrap();
            assert_eq!(ppu.read(base, &cart).unwrap(), value);
            assert_eq!(ppu.read(alias + 0x20, &cart).unwrap(), value);

            ppu.write(base, value + 8, &mut cart).unwrap();
            assert_eq!(ppu.read(alias, &cart).unwrap(), value + 8);
        }

        // 不同的特殊别名组不能全部落到 $3F00。
        assert_eq!(ppu.read(0x3F00, &cart).unwrap(), 9);
        assert_eq!(ppu.read(0x3F04, &cart).unwrap(), 10);

        // 普通背景/精灵调色板槽位相互独立。
        ppu.write(0x3F01, 0x15, &mut cart).unwrap();
        ppu.write(0x3F11, 0x26, &mut cart).unwrap();
        assert_eq!(ppu.read(0x3F21, &cart).unwrap(), 0x15);
        assert_eq!(ppu.read(0x3F31, &cart).unwrap(), 0x26);

        ppu.write(0x3FFF, 0xFF, &mut cart).unwrap();
        assert_eq!(ppu.read(0x3F1F, &cart).unwrap(), 0x3F);

        // 调色板写入不能破坏名称表。
        ppu.write(0x2F00, 0xAB, &mut cart).unwrap();
        ppu.write(0x3F00, 0x12, &mut cart).unwrap();
        assert_eq!(ppu.read(0x2F00, &cart).unwrap(), 0xAB);
    }
}
