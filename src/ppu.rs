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

    ctrl: u8,
    status: u8,

    v: u16,            // 当前 VRAM 地址，内部保留 15 位
    t: u16,            // 临时地址，后续滚动也会使用
    write_latch: bool, // false：下一次写地址高字节；true：写低字节

    read_buffer: u8, // PPUDATA 的延迟读取缓冲
    io_latch: u8,    // 最近一次寄存器接口的数据值，简化模拟 open bus
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            dot: 0,
            scanline: 0,
            frame: 0,
            nametable_ram: [0u8; 2048],
            palette_ram: [0u8; 32],
            ctrl: 0,
            status: 0,
            v: 0,
            t: 0,
            write_latch: false,
            read_buffer: 0,
            io_latch: 0,
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
            _ => unreachable!("address was masked to 14 bits"),
        }
        Ok(())
    }

    // addr 是 CPU 访问 PPU 寄存器的地址。
    pub fn cpu_read(&mut self, addr: u16, cartridge: &Cartridge) -> Result<u8, String> {
        let addr = addr & 0x07;
        let value = match addr {
            0x0002 => {
                let v = (self.status & 0xE0) | (self.io_latch & 0x1F);
                self.status &= 0b0111_1111;
                self.write_latch = false;
                v
            }
            0x0000 | 0x0006 => self.io_latch,
            0x0007 => {
                // 如果是从nametable中读取，会有一个周期的延迟，也就是说read_buffer保存着上次读取的值还没有被使用，
                // 所以需要先保存上一个周期读取的数据。
                // 如果是从palette中读取直接返回,同时用 addr - 0x1000 对应的名称表数据更新 read_buffer
                let addr = self.v & 0x3FFF;

                let value = if addr >= 0x3F00 {
                    // 调色板立即返回；缓冲从下方名称表地址补充。
                    let palette = self.read(addr, cartridge)?;
                    self.read_buffer = self.read(addr - 0x1000, cartridge)?;

                    (palette & 0x3F) | (self.io_latch & 0xC0)
                } else {
                    // 返回旧缓冲，再装入当前地址的数据。
                    let old = self.read_buffer;
                    self.read_buffer = self.read(addr, cartridge)?;
                    old
                };

                let increment = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
                self.v = self.v.wrapping_add(increment) & 0x7FFF;

                value
            }
            _ => return Err(format!("unimplemented cpu read addr: {:0x}", addr)),
        };

        self.io_latch = value;
        Ok(value)
    }

    pub fn cpu_write(
        &mut self,
        addr: u16,
        value: u8,
        cartridge: &mut Cartridge,
    ) -> Result<(), String> {
        let addr = addr & 0x07;
        self.io_latch = value;
        match addr {
            0x0000 => {
                self.ctrl = value;
                // 控制寄存器的nametable_x(bit 0)写入 t寄存器的nametable_x(bit 10)
                // 控制寄存器的nametable_y(bit 1)写入 t寄存器的nametable_y(bit 11)
                self.t = (self.t & !0x0C00) | (u16::from(value & 0x03) << 10);
            }
            0x0002 => {}
            0x0006 => {
                if self.write_latch {
                    self.t = (self.t & 0xFF00) | u16::from(value);
                    self.v = self.t;
                    self.write_latch = false;
                } else {
                    self.t = u16::from(value & 0x3F) << 8 | (self.t & 0x00FF);
                    // 清除bit 14
                    self.t &= 0xBFFF;
                    self.write_latch = true;
                }
            }
            0x0007 => {
                self.write(self.v & 0x3FFF, value, cartridge)?;
                // 根据控制寄存器中的increment位(bit 2)的值决定增加v寄存器的coarse_x(bit 0-4)还是coarse_y(bit 5-9)
                let increment = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
                self.v = self.v.wrapping_add(increment) & 0x7FFF;
            }
            _ => return Err(format!("unimplemented PPU register write: {addr:#04X}")),
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

    fn set_ppu_address(ppu: &mut Ppu, cart: &mut Cartridge, addr: u16) {
        ppu.cpu_write(0x2006, (addr >> 8) as u8, cart).unwrap();
        ppu.cpu_write(0x2006, addr as u8, cart).unwrap();
    }

    #[test]
    fn ppu_register_data_write_increments_by_one_or_32() {
        let mut cart = ppu_test_cartridge(true, false);
        let mut ppu = Ppu::new();

        set_ppu_address(&mut ppu, &mut cart, 0x2000);
        ppu.cpu_write(0x2007, 0xAA, &mut cart).unwrap();
        ppu.cpu_write(0x2007, 0xBB, &mut cart).unwrap();

        assert_eq!(ppu.read(0x2000, &cart).unwrap(), 0xAA);
        assert_eq!(ppu.read(0x2001, &cart).unwrap(), 0xBB);
        assert_eq!(ppu.v, 0x2002);

        ppu.cpu_write(0x2000, 0x04, &mut cart).unwrap();
        set_ppu_address(&mut ppu, &mut cart, 0x2100);

        ppu.cpu_write(0x2007, 0x11, &mut cart).unwrap();
        ppu.cpu_write(0x2007, 0x22, &mut cart).unwrap();

        assert_eq!(ppu.read(0x2100, &cart).unwrap(), 0x11);
        assert_eq!(ppu.read(0x2120, &cart).unwrap(), 0x22);
        assert_eq!(ppu.v, 0x2140);
    }

    #[test]
    fn ppu_register_palette_read_is_immediate_and_refills_buffer() {
        let mut cart = ppu_test_cartridge(true, false);
        let mut ppu = Ppu::new();

        ppu.write(0x3F00, 0x2A, &mut cart).unwrap();
        ppu.write(0x2F00, 0x55, &mut cart).unwrap();
        ppu.write(0x2000, 0x77, &mut cart).unwrap();

        set_ppu_address(&mut ppu, &mut cart, 0x3F00);

        // 设置 I/O latch 高两位；写 STATUS 不改变状态。
        ppu.cpu_write(0x2002, 0xC0, &mut cart).unwrap();

        assert_eq!(ppu.cpu_read(0x2007, &cart).unwrap(), 0xEA);
        assert_eq!(ppu.v, 0x3F01);

        set_ppu_address(&mut ppu, &mut cart, 0x2000);

        // 改地址不会清除读取缓冲。
        assert_eq!(ppu.cpu_read(0x2007, &cart).unwrap(), 0x55);
        assert_eq!(ppu.cpu_read(0x2007, &cart).unwrap(), 0x77);
    }

    #[test]
    fn ppu_register_status_clears_vblank_and_address_latch() {
        let mut cart = ppu_test_cartridge(false, false);
        let mut ppu = Ppu::new();

        ppu.status = 0xE0;
        ppu.read_buffer = 0xAB;

        ppu.cpu_write(0x2006, 0x3F, &mut cart).unwrap();
        assert!(ppu.write_latch);

        // 返回旧状态高三位 + 最近写入值的低五位。
        assert_eq!(ppu.cpu_read(0x2002, &cart).unwrap(), 0xFF);
        assert_eq!(ppu.status, 0x60);
        assert!(!ppu.write_latch);
        assert_eq!(ppu.read_buffer, 0xAB);

        set_ppu_address(&mut ppu, &mut cart, 0x2100);
        ppu.cpu_write(0x2007, 0x42, &mut cart).unwrap();

        assert_eq!(ppu.read(0x2100, &cart).unwrap(), 0x42);
    }

    #[test]
    fn ppu_register_address_and_ctrl_update_correct_bits() {
        let mut cart = ppu_test_cartridge(false, false);
        let mut ppu = Ppu::new();

        ppu.t = 0x73AA;
        ppu.v = 0x1234;

        ppu.cpu_write(0x2000, 0x03, &mut cart).unwrap();
        assert_eq!(ppu.t, 0x7FAA);
        assert_eq!(ppu.v, 0x1234);

        ppu.cpu_write(0x2006, 0xFF, &mut cart).unwrap();
        assert_eq!(ppu.t, 0x3FAA); // 高字节仅保留六位。
        assert_eq!(ppu.v, 0x1234); // 第一次写不更新 v。

        ppu.cpu_write(0x2006, 0xFF, &mut cart).unwrap();
        assert_eq!(ppu.v, 0x3FFF);

        ppu.cpu_write(0x2007, 0x2A, &mut cart).unwrap();
        assert_eq!(ppu.v, 0x4000); // 内部 v 保留 15 位。

        ppu.cpu_write(0x2007, 0x66, &mut cart).unwrap();
        assert_eq!(ppu.read(0x0000, &cart).unwrap(), 0x66);
    }

    #[test]
    fn ppu_register_bus_mirrors_cpu_register_addresses() {
        let cart = ppu_test_cartridge(false, false);
        let mut bus = crate::bus::Bus::new();
        bus.insert_cartridge(cart);

        // $200E 是 $2006 的镜像，$200F 是 $2007 的镜像。
        bus.cpu_write(0x200E, 0x20).unwrap();
        bus.cpu_write(0x200E, 0x00).unwrap();
        bus.cpu_write(0x200F, 0x5A).unwrap();

        bus.cpu_write(0x2006, 0x20).unwrap();
        bus.cpu_write(0x2006, 0x00).unwrap();

        assert_eq!(bus.cpu_read(0x200F).unwrap(), 0); // 初始缓冲
        assert_eq!(bus.cpu_read(0x2007).unwrap(), 0x5A);
    }
}
