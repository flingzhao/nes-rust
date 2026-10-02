use sdl2::pixels;

use crate::cartridge::{Cartridge, Mirroring};
use crate::debug_video::decode_tile_row;

const COARSE_X_MASK: u16 = 0x001F;
const COARSE_Y_MASK: u16 = 0x03E0;
const FINE_Y_MASK: u16 = 0x7000;
const SCROLL_X_MASK: u16 = 0x041F;
const SCROLL_Y_MASK: u16 = 0x7BE0;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpriteRow {
    oam_index: u8,
    x: u8,
    palette: u8,
    behind_background: bool,
    pixels: [u8; 8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpritePixel {
    pixel: u8,
    palette: u8,
    behind_background: bool,
    is_sprite_zero: bool,
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

    fine_x: u8,

    // 正在取得的下一个图块数据。
    next_tile_id: u8,
    next_tile_palette: u8, // 已选出象限的子调色板编号：0～3
    next_tile_pattern_low: u8,
    next_tile_pattern_high: u8,

    // 图案移位寄存器。
    bg_pattern_low: u16,
    bg_pattern_high: u16,

    // 子调色板编号的两个位平面。
    bg_attribute_low: u16,
    bg_attribute_high: u16,
    mask: u8,
    framebuffer: Vec<u8>,
    oam: [u8; 256],
    oam_addr: u8,

    sprite_rows: Vec<SpriteRow>,      // 当前扫描线
    next_sprite_rows: Vec<SpriteRow>, // 下一条扫描线
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
            fine_x: 0,
            next_tile_id: 0,
            next_tile_palette: 0,
            next_tile_pattern_low: 0,
            next_tile_pattern_high: 0,
            bg_pattern_low: 0,
            bg_pattern_high: 0,
            bg_attribute_low: 0u16,
            bg_attribute_high: 0u16,
            mask: 0,
            framebuffer: vec![0; 256 * 240],
            oam: [0u8; 256],
            oam_addr: 0,
            sprite_rows: Vec::<SpriteRow>::new(),
            next_sprite_rows: Vec::<SpriteRow>::new(),
        }
    }

    #[cfg(test)]
    pub fn clock_test(&mut self) {
        if self.dot == 1 && self.scanline == 241 {
            self.status = self.status | 0x80;
        }

        if self.dot == 1 && self.scanline == 261 {
            self.status = self.status & 0x1F;
        }

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

    /// 推进一个 PPU 周期。
    pub fn clock(&mut self, cartridge: Option<&Cartridge>) -> Result<(), String> {
        if self.dot == 1 && self.scanline == 241 {
            self.status = self.status | 0x80;
        }

        if self.dot == 1 && self.scanline == 261 {
            self.status = self.status & 0x1F;
        }

        if self.scanline == 261 && self.dot == 1 {
            self.status &= !0xE0;
        }

        // render background and sprite
        // 可见区域：背景和精灵合成后输出。
        if self.scanline < 240 && (1..=256).contains(&self.dot) {
            let x = self.dot - 1;
            let y = usize::from(self.scanline);

            let (background_pixel, background_palette) = self.background_pixel();

            let palette_addr = self.compose_pixel(x, background_pixel, background_palette);

            let color = self.palette_ram[palette_index(palette_addr)];

            let color_mask = if self.mask & 0x01 != 0 {
                0x30 // 灰度模式
            } else {
                0x3F
            };

            self.framebuffer[y * 256 + usize::from(x)] = color & color_mask;
        }

        let render_line = self.scanline < 240 || self.scanline == 261;
        let rendering_enabled = self.mask & 0x18 != 0;

        if render_line && rendering_enabled {
            let fetch_window = (1..=256).contains(&self.dot) || (321..=336).contains(&self.dot);

            if fetch_window {
                let cartridge =
                    cartridge.ok_or_else(|| "No cartridge for PPU rendering".to_string())?;

                self.shift_background();

                match self.dot % 8 {
                    2 => {
                        // 读取图块编号。
                        self.fetch_bg_tile_id(cartridge)?;
                    }
                    4 => {
                        // 读取属性。
                        self.fetch_bg_attribute(cartridge)?;
                    }
                    6 => {
                        // 读取图案低位平面。
                        self.fetch_bg_pattern_low(cartridge)?;
                    }
                    0 => {
                        // 读取高位平面。
                        self.fetch_bg_pattern_high(cartridge)?;
                        // 装载移位寄存器。
                        // 横向地址递增。
                        self.load_background_shifters();
                        self.increment_scroll_x();
                    }
                    _ => {}
                }
            }

            // 分别判断纵向递增、横向复制、纵向复制。
            if self.dot == 256 {
                self.increment_scroll_y();
            }

            if self.dot == 257 {
                self.copy_scroll_x();
            }

            if self.scanline == 261 && (280..=304).contains(&self.dot) {
                self.copy_scroll_y();
            }
        }

        self.clock_sprites(cartridge)?;

        self.dot += 1;
        if self.dot == 341_u16 {
            self.dot = 0;
            self.scanline += 1;
        }

        if self.scanline == 262 {
            self.scanline = 0;
            self.frame += 1;
        }
        Ok(())
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

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    pub fn nmi_line(&self) -> bool {
        return self.status & 0x80 != 0 && self.ctrl & 0x80 != 0;
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

    /// 渲染关闭时，PPUDATA 访问后的地址递增。
    // 根据控制寄存器中的increment位(bit 2)的值决定增加v寄存器的coarse_x(bit 0-4)还是coarse_y(bit 5-9)
    fn increment_vram_address(&mut self) {
        let increment = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
        self.v = self.v.wrapping_add(increment) & 0x7FFF;
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
            0x0004 => self.oam[usize::from(self.oam_addr)],
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

                self.increment_vram_address();

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
            0x0001 => {
                self.mask = value;
            }
            0x0002 => {}
            0x0003 => {
                self.oam_addr = value;
            }
            0x0004 => {
                let stored = if self.oam_addr & 0x03 == 2 {
                    value & 0xE3
                } else {
                    value
                };

                self.oam[usize::from(self.oam_addr)] = stored;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            }
            0x0005 => {
                // 位：  14—12    11      10       9—5       4—0
                // 含义：fine Y   纵向表  横向表   coarse Y  coarse X
                if self.write_latch == false {
                    // self.t = (self.t & 0xFFE0) | u16::from(value >> 3); // t.coarse_X = value >> 3
                    self.t = (self.t & !COARSE_X_MASK) | u16::from(value >> 3); // t.coarse_X = value >> 3
                    self.fine_x = value & 0x07;
                    self.write_latch = true;
                } else {
                    // self.t = (self.t & 0b_1111_1100_0001_1111) | u16::from(value >> 3); // t.coarse_y = value >> 3
                    self.t = (self.t & !COARSE_Y_MASK) | u16::from(value >> 3) << 5; // t.coarse_y = value >> 3
                    // 替换bit12-14
                    self.t = (self.t & !FINE_Y_MASK) | u16::from(value & 0x07) << 12; // t.fine_y = value >> 3
                    self.write_latch = false;
                }
            }
            0x0006 => {
                if self.write_latch == false {
                    self.t = u16::from(value & 0x3F) << 8 | (self.t & 0x00FF);
                    // 清除bit 14
                    self.t &= 0xBFFF;
                    self.write_latch = true;
                } else {
                    self.t = (self.t & 0xFF00) | u16::from(value);
                    self.v = self.t;
                    self.write_latch = false;
                }
            }
            0x0007 => {
                self.write(self.v & 0x3FFF, value, cartridge)?;
                self.increment_vram_address();
            }
            _ => return Err(format!("unimplemented PPU register write: {addr:#04X}")),
        }
        Ok(())
    }

    // 前进一个图块(8*8)
    fn increment_scroll_x(&mut self) {
        match self.v & COARSE_X_MASK {
            0x0000..=0x001E => self.v += 1,
            0x001F => {
                self.v = self.v & !COARSE_X_MASK;
                self.v = self.v ^ (1_u16 << 10); // flip bit 10
            }
            _ => unreachable!(),
        }
    }

    // 前进一行像素
    fn increment_scroll_y(&mut self) {
        let fine_y = (self.v >> 12) & 0x07;
        match fine_y {
            0x00..=0x06 => self.v = (self.v & !FINE_Y_MASK) | (fine_y + 1) << 12,
            0x07 => {
                self.v = self.v & !FINE_Y_MASK;
                // 处理coarse_y
                let coarse_y = (self.v & COARSE_Y_MASK) >> 5;
                match coarse_y {
                    29 => {
                        self.v = self.v ^ (1_u16 << 11); // flip bit 11
                        self.v &= !COARSE_Y_MASK; // coarse_y = 0
                    }
                    31 => {
                        self.v &= !COARSE_Y_MASK; // coarse_y = 0
                    }
                    _ => {
                        self.v = (self.v & !COARSE_Y_MASK) | (coarse_y + 1) << 5; // coarse_y += 1
                    }
                }
            }
            _ => unreachable!(),
        }
    }

    fn copy_scroll_x(&mut self) {
        self.v = (self.v & !SCROLL_X_MASK) | (self.t & SCROLL_X_MASK);
    }

    fn copy_scroll_y(&mut self) {
        self.v = (self.v & !SCROLL_Y_MASK) | (self.t & SCROLL_Y_MASK);
    }

    fn fetch_bg_tile_id(&mut self, cartridge: &Cartridge) -> Result<(), String> {
        let addr = 0x2000 | (self.v & 0x0FFF);
        self.next_tile_id = self.read(addr, cartridge)?;
        Ok(())
    }

    fn fetch_bg_attribute(&mut self, cartridge: &Cartridge) -> Result<(), String> {
        let attribute_addr =
            0x23C0 | (self.v & 0x0C00) | ((self.v >> 4) & 0x0038) | ((self.v >> 2) & 0x0007);
        let attribute = self.read(attribute_addr, cartridge)?;
        let tile_x = self.v & 0x001F;
        let tile_y = (self.v >> 5) & 0x001F;
        let right = tile_x % 4 >= 2;
        let bottom = tile_y % 4 >= 2;

        let shift = match (bottom, right) {
            (false, false) => 0,
            (false, true) => 2,
            (true, false) => 4,
            (true, true) => 6,
        };
        let palette_id = (attribute >> shift) & 0x03;

        self.next_tile_palette = palette_id;
        Ok(())
    }

    fn fetch_bg_pattern_low(&mut self, cartridge: &Cartridge) -> Result<(), String> {
        let pattern_base: u16 = if self.ctrl & 0x10 != 0 {
            0x1000
        } else {
            0x0000
        };

        let fine_y = (self.v >> 12) & 0x07;

        let addr = pattern_base + u16::from(self.next_tile_id) * 16 + fine_y;
        self.next_tile_pattern_low = self.read(addr, cartridge)?;
        Ok(())
    }

    fn fetch_bg_pattern_high(&mut self, cartridge: &Cartridge) -> Result<(), String> {
        let pattern_base: u16 = if self.ctrl & 0x10 != 0 {
            0x1000
        } else {
            0x0000
        };

        let fine_y = (self.v >> 12) & 0x07;

        let addr = pattern_base + u16::from(self.next_tile_id) * 16 + fine_y + 8;
        self.next_tile_pattern_high = self.read(addr, cartridge)?;
        Ok(())
    }

    // 返回：(图块像素编号 0～3，子调色板编号 0～3)
    fn background_pixel(&self) -> (u8, u8) {
        let mask = 0x8000_u16 >> self.fine_x;

        // 两个位平面分别提供像素编号的 bit 0 和 bit 1。
        let pixel_low = u8::from(self.bg_pattern_low & mask != 0);
        let pixel_high = u8::from(self.bg_pattern_high & mask != 0);

        // 配色同样随像素移动，取相同位置的两位。
        let palette_low = u8::from(self.bg_attribute_low & mask != 0);
        let palette_high = u8::from(self.bg_attribute_high & mask != 0);

        let pixel = (pixel_high << 1) | pixel_low;
        let palette = (palette_high << 1) | palette_low;

        (pixel, palette)
    }

    fn load_background_shifters(&mut self) {
        // 保留高 8 位，将取得的新图块行装入低 8 位。
        self.bg_pattern_low =
            (self.bg_pattern_low & 0xFF00) | u16::from(self.next_tile_pattern_low);

        self.bg_pattern_high =
            (self.bg_pattern_high & 0xFF00) | u16::from(self.next_tile_pattern_high);

        // 同一图块的八个像素使用同一个子调色板。
        // 把子调色板编号的每一位扩展成八个相同的位。
        let attribute_low: u16 = if self.next_tile_palette & 0x01 != 0 {
            0xFF
        } else {
            0
        };

        let attribute_high: u16 = if self.next_tile_palette & 0x02 != 0 {
            0xFF
        } else {
            0
        };

        self.bg_attribute_low = (self.bg_attribute_low & 0xFF00) | attribute_low;

        self.bg_attribute_high = (self.bg_attribute_high & 0xFF00) | attribute_high;
    }

    fn shift_background(&mut self) {
        // 图案和配色一起移动，保持像素与子调色板对应。
        self.bg_pattern_low <<= 1;
        self.bg_pattern_high <<= 1;
        self.bg_attribute_low <<= 1;
        self.bg_attribute_high <<= 1;
    }

    pub fn oam(&self) -> &[u8; 256] {
        &self.oam
    }

    // row 是已经处理过上下翻转的行号：
    // 8×8 时为 0～7，8×16 时为 0～15。
    fn sprite_pattern_address(&self, tile_id: u8, row: u8) -> u16 {
        let sprite_mode8x8 = self.ctrl & 0x20 == 0;
        let addr = match sprite_mode8x8 {
            true => {
                let table_base = if self.ctrl & 0x08 == 0 {
                    0x0000_u16
                } else {
                    0x1000_u16
                };

                table_base + u16::from(tile_id) * 16_u16 + u16::from(row)
            }
            false => {
                let table_base = u16::from(tile_id & 0x01) * 0x1000;
                let top_tile = u16::from(tile_id & 0xFE);
                let actual_tile = top_tile + u16::from(row) / 8;

                table_base + actual_tile * 16 + u16::from(row) % 8
            }
        };

        addr
    }

    // scanline 明确表示“需要准备像素的目标扫描线”。
    fn collect_sprite_rows(
        &self,
        scanline: u16,
        cartridge: &Cartridge,
    ) -> Result<Vec<SpriteRow>, String> {
        if scanline >= 240 {
            return Ok(Vec::<SpriteRow>::new());
        }

        let height: u16 = if self.ctrl & 0x20 != 0 { 16 } else { 8 };
        let mut sprites = Vec::new();

        for index in 0..64 {
            let oam_y = self.oam[index * 4usize];
            let oma_index = self.oam[index * 4usize + 1];
            let attr = self.oam[index * 4usize + 2];
            let oam_x = self.oam[index * 4usize + 3];

            let flipy = attr & 0x80 != 0;
            let flipx = attr & 0x40 != 0;
            let behind_background = attr & 0x20 != 0;
            let palette = attr & 0x03;

            // Y坐标有一行偏移
            let top = u16::from(oam_y) + 1;
            // 先判断目标扫描线是否落在精灵范围内。
            if scanline < top || scanline >= top + height {
                continue;
            }

            let screen_row = scanline - top;

            let pattern_row = if attr & 0x80 != 0 {
                height - 1 - screen_row
            } else {
                screen_row
            };

            let low_addr = self.sprite_pattern_address(oma_index, pattern_row as u8);
            let high_addr = low_addr + 8;

            let low = self.read(low_addr, cartridge)?;
            let high = self.read(high_addr, cartridge)?;

            let mut pixels = decode_tile_row(low, high);

            let mut pixels = decode_tile_row(low, high);

            if flipx {
                pixels.reverse();
            }
            sprites.push(SpriteRow {
                oam_index: index as u8,
                x: oam_x,
                palette: palette,
                behind_background: behind_background,
                pixels: pixels,
            });

            if sprites.len() == 8 {
                break;
            }
        }

        Ok(sprites)
    }

    // 取得当前 X 的精灵像素
    fn sprite_pixel(&self, x: u16) -> Option<SpritePixel> {
        if x >= 256 {
            return None;
        }

        for row in &self.sprite_rows {
            let sprite_x = u16::from(row.x);
            let in_range = (sprite_x..sprite_x + 8).contains(&x);
            if !in_range {
                continue;
            }
            let pixel = row.pixels[usize::from(x) - usize::from(row.x)];
            if pixel == 0 {
                continue;
            }
            return Some(SpritePixel {
                pixel: pixel,
                palette: row.palette,
                behind_background: row.behind_background,
                is_sprite_zero: row.oam_index == 0,
            });
        }

        None
    }

    // 合成并返回调色板地址
    fn compose_pixel(&mut self, x: u16, background_pixel: u8, background_palette: u8) -> u16 {
        if x >= 256 {
            return 0x3F00;
        }

        // 背景是否允许在这个位置显示。
        let background_visible = self.mask & 0x08 != 0 && (x >= 8 || self.mask & 0x02 != 0);

        let background_pixel = if background_visible {
            background_pixel
        } else {
            0
        };

        // 精灵是否允许在这个位置显示。
        let sprite_visible = self.mask & 0x10 != 0 && (x >= 8 || self.mask & 0x04 != 0);

        let sprite = if sprite_visible {
            self.sprite_pixel(x)
        } else {
            None
        };

        if let Some(sprite) = sprite {
            // sprite_pixel() 保证返回的精灵像素非透明。
            // 命中与精灵在背景前面还是后面无关。
            if sprite.is_sprite_zero && background_pixel != 0 && x != 255 {
                self.status |= 0x40;
            }

            // 背景透明，或者精灵位于背景前面：显示精灵。
            if background_pixel == 0 || !sprite.behind_background {
                return 0x3F10 + u16::from(sprite.palette) * 4 + u16::from(sprite.pixel);
            }
        }

        // 没有精灵，或者选中的精灵被不透明背景遮挡。
        if background_pixel != 0 {
            0x3F00 + u16::from(background_palette) * 4 + u16::from(background_pixel)
        } else {
            0x3F00
        }
    }

    fn clock_sprites(&mut self, cartridge: Option<&Cartridge>) -> Result<(), String> {
        // 新的可见扫描线开始：接收上一行准备好的数据。
        if self.scanline < 240 && self.dot == 0 {
            self.sprite_rows = std::mem::take(&mut self.next_sprite_rows);
        }

        if self.dot == 257 {
            // 防止没有准备新数据时复用旧数据。
            self.next_sprite_rows.clear();

            let rendering_enabled = self.mask & 0x18 != 0;

            // 第 0～238 行分别准备第 1～239 行。
            // 不为非可见行或第 0 行准备精灵。
            if self.scanline < 239 && rendering_enabled {
                let cartridge =
                    cartridge.ok_or_else(|| "No cartridge for sprite rendering".to_string())?;

                self.next_sprite_rows = self.collect_sprite_rows(self.scanline + 1, cartridge)?;
            }
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
            ppu.clock_test();
        }

        assert_eq!(ppu.dot(), 340);
        assert_eq!(ppu.scanline(), 0);
        assert_eq!(ppu.frame(), 0);

        ppu.clock_test();

        assert_eq!(ppu.dot(), 0);
        assert_eq!(ppu.scanline(), 1);
        assert_eq!(ppu.frame(), 0);
    }

    #[test]
    fn ppu_timing_wraps_frames_without_rendering() {
        let mut ppu = Ppu::new();

        for _ in 0..(341 * 262 - 1) {
            ppu.clock_test();
        }

        assert_eq!(ppu.dot(), 340);
        assert_eq!(ppu.scanline(), 261);
        assert_eq!(ppu.frame(), 0);

        ppu.clock_test();

        assert_eq!(ppu.dot(), 0);
        assert_eq!(ppu.scanline(), 0);
        assert_eq!(ppu.frame(), 1);

        // 未开启渲染，第二帧同样是完整的 89342 个周期。
        for _ in 0..(341 * 262) {
            ppu.clock_test();
        }

        assert_eq!(ppu.dot(), 0);
        assert_eq!(ppu.scanline(), 0);
        assert_eq!(ppu.frame(), 2);
    }

    use crate::cartridge::{self, Cartridge};

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

    #[test]
    fn ppu_vblank_starts_at_241_dot_1() {
        let mut ppu = Ppu::new();

        // 到达待处理位置 (241, 1)，尚未处理该 tick。
        for _ in 0..(241 * 341 + 1) {
            ppu.clock_test();
        }

        assert_eq!((ppu.scanline(), ppu.dot()), (241, 1));
        assert_eq!(ppu.status & 0x80, 0);

        ppu.clock_test();

        assert_eq!((ppu.scanline(), ppu.dot()), (241, 2));
        assert_ne!(ppu.status & 0x80, 0);

        // 未开启 NMI，但 VBlank 标志仍然会设置。
        assert!(!ppu.nmi_line());
    }

    #[test]
    fn ppu_vblank_prerender_clears_status_flags() {
        let mut ppu = Ppu::new();

        for _ in 0..(261 * 341 + 1) {
            ppu.clock_test();
        }

        assert_eq!((ppu.scanline(), ppu.dot()), (261, 1));

        // 同时验证 Sprite 0 Hit / Overflow 的清除。
        ppu.status |= 0xE0;
        ppu.clock_test();

        assert_eq!(ppu.status & 0xE0, 0);
        assert!(!ppu.nmi_line());
    }

    #[test]
    fn ppu_vblank_nmi_line_tracks_ctrl_and_status() {
        let mut cart = ppu_test_cartridge(false, false);
        let mut ppu = Ppu::new();

        // 没有 VBlank，单独打开 NMI 不产生请求。
        ppu.cpu_write(0x2000, 0x80, &mut cart).unwrap();
        assert!(!ppu.nmi_line());

        for _ in 0..(241 * 341 + 2) {
            ppu.clock_test();
        }
        assert!(ppu.nmi_line());

        // 关闭 NMI 不清除 VBlank。
        ppu.cpu_write(0x2000, 0x00, &mut cart).unwrap();
        assert!(!ppu.nmi_line());
        assert_ne!(ppu.status & 0x80, 0);

        // VBlank 期间重新打开开关，立即重新产生有效状态。
        ppu.cpu_write(0x2000, 0x80, &mut cart).unwrap();
        assert!(ppu.nmi_line());

        // STATUS 返回旧的 VBlank 标志，随后清除它。
        let status = ppu.cpu_read(0x2002, &cart).unwrap();
        assert_ne!(status & 0x80, 0);
        assert!(!ppu.nmi_line());

        // 读取之后继续推进，不应在同一 VBlank 内自动重新置位。
        for _ in 0..20 {
            ppu.clock_test();
        }
        assert_eq!(ppu.status & 0x80, 0);
    }

    #[cfg(test)]
    mod scroll_tests {
        use super::*;
        // 如果父模块没有导入 Cartridge，在这里导入它。

        fn cartridge() -> Cartridge {
            let mut bytes = vec![0u8; 16 + 16 * 1024];
            bytes[..4].copy_from_slice(b"NES\x1A");
            bytes[4] = 1; // 16 KiB PRG，CHR RAM
            Cartridge::try_from(bytes.as_slice()).unwrap()
        }

        #[test]
        fn scroll_writes_replace_fields_without_changing_v() {
            let mut ppu = Ppu::new();
            let mut cart = cartridge();

            ppu.v = 0x1234;
            ppu.cpu_write(0x2000, 3, &mut cart).unwrap();

            ppu.cpu_write(0x2005, 19, &mut cart).unwrap();
            assert_eq!(ppu.fine_x, 3);
            assert_eq!(ppu.t, 0x0C02);
            assert!(ppu.write_latch);
            assert_eq!(ppu.v, 0x1234);

            ppu.cpu_write(0x2005, 43, &mut cart).unwrap();
            assert_eq!(ppu.t, 0x3CA2);
            assert!(!ppu.write_latch);
            assert_eq!(ppu.v, 0x1234);

            // 再写零，必须清除旧滚动位，但保留名称表位。
            ppu.cpu_write(0x2005, 0, &mut cart).unwrap();
            ppu.cpu_write(0x2005, 0, &mut cart).unwrap();

            assert_eq!(ppu.t, 0x0C00);
            assert_eq!(ppu.fine_x, 0);
            assert_eq!(ppu.v, 0x1234);
        }

        #[test]
        fn scroll_and_address_share_the_write_latch() {
            let mut ppu = Ppu::new();
            let mut cart = cartridge();

            // 第一次写 2005，使下一次成为“第二次写”。
            ppu.cpu_write(0x2005, 19, &mut cart).unwrap();

            // 因为共用 latch，这次是 2006 的低字节写入。
            ppu.cpu_write(0x2006, 0x34, &mut cart).unwrap();

            assert_eq!(ppu.t, 0x0034);
            assert_eq!(ppu.v, 0x0034);
            assert_eq!(ppu.fine_x, 3);
            assert!(!ppu.write_latch);

            ppu.cpu_write(0x2005, 7, &mut cart).unwrap();
            assert!(ppu.write_latch);

            // 读状态寄存器，把下一次写重置为第一次。
            ppu.cpu_read(0x2002, &cart).unwrap();
            assert!(!ppu.write_latch);

            ppu.cpu_write(0x2005, 8, &mut cart).unwrap();
            assert_eq!(ppu.t & 0x001F, 1);
            assert_eq!(ppu.fine_x, 0);
            assert!(ppu.write_latch);
        }

        #[test]
        fn horizontal_increment_wraps_and_preserves_other_fields() {
            for (before, expected) in [
                (0x0000u16, 0x0001u16),
                (0x001E, 0x001F),
                (0x001F, 0x0400),
                (0x041F, 0x0000),
                (0x7BFE, 0x7BFF),
                (0x7BFF, 0x7FE0),
            ] {
                let mut ppu = Ppu::new();
                ppu.v = before;
                ppu.t = 0x1234;
                ppu.fine_x = 5;

                ppu.increment_scroll_x();

                assert_eq!(ppu.v, expected, "before={before:04X}");
                assert_eq!(ppu.t, 0x1234);
                assert_eq!(ppu.fine_x, 5);
            }
        }

        #[test]
        fn vertical_increment_handles_fine_y_and_special_rows() {
            for (before, expected) in [
                (0x0000u16, 0x1000u16), // fine Y: 0 → 1
                (0x6000, 0x7000),       // fine Y: 6 → 7
                (0x7000, 0x0020),       // coarse Y: 0 → 1
                (0x73A0, 0x0800),       // coarse Y: 29 → 0，切换表
                (0x7BA0, 0x0000),       // 同上，切回原表
                (0x73C0, 0x03E0),       // coarse Y: 30 → 31
                (0x73E0, 0x0000),       // coarse Y: 31 → 0，不切表
                (0x7BE0, 0x0800),       // 保留原纵向表位
            ] {
                let mut ppu = Ppu::new();

                // 同时设置横向字段，检查它们不被误改。
                ppu.v = before | 0x0415;
                ppu.t = 0x1234;
                ppu.fine_x = 6;

                ppu.increment_scroll_y();

                assert_eq!(ppu.v, expected | 0x0415, "before={before:04X}");
                assert_eq!(ppu.t, 0x1234);
                assert_eq!(ppu.fine_x, 6);
            }
        }

        #[test]
        fn scroll_copies_replace_only_the_selected_fields() {
            let mut ppu = Ppu::new();
            ppu.fine_x = 5;

            ppu.v = 0x7FFF;
            ppu.t = 0;
            ppu.copy_scroll_x();
            assert_eq!(ppu.v, 0x7BE0);

            ppu.t = 0x041F;
            ppu.copy_scroll_x();
            assert_eq!(ppu.v, 0x7FFF);
            assert_eq!(ppu.t, 0x041F);

            ppu.t = 0;
            ppu.copy_scroll_y();
            assert_eq!(ppu.v, 0x041F);

            ppu.t = 0x7BE0;
            ppu.copy_scroll_y();
            assert_eq!(ppu.v, 0x7FFF);
            assert_eq!(ppu.t, 0x7BE0);
            assert_eq!(ppu.fine_x, 5);
        }
    }

    #[cfg(test)]
    mod background_pipeline_tests {
        use super::*;

        fn cartridge() -> Cartridge {
            let mut bytes = vec![0u8; 16 + 16 * 1024];
            bytes[..4].copy_from_slice(b"NES\x1A");
            bytes[4] = 1;
            bytes[6] = 1; // 垂直镜像，CHR RAM
            Cartridge::try_from(bytes.as_slice()).unwrap()
        }

        #[test]
        fn fetches_tile_attribute_and_pattern_from_v() {
            let mut ppu = Ppu::new();
            let mut cart = cartridge();

            // 名称表 2400，coarse X=10，coarse Y=6，fine Y=3。
            ppu.v = 0x3000 | 0x0400 | (6 << 5) | 10;
            ppu.t = 0x1234;
            ppu.fine_x = 5;
            ppu.read_buffer = 0xA7;
            ppu.write_latch = true;
            ppu.ctrl = 0x10; // 背景使用图案表 1000。

            ppu.write(0x24CA, 5, &mut cart).unwrap();
            ppu.write(0x27CA, 0xE4, &mut cart).unwrap();

            cart.ppu_write(0x1053, 0x96).unwrap();
            cart.ppu_write(0x105B, 0x69).unwrap();

            let old_v = ppu.v;

            ppu.fetch_bg_tile_id(&cart).unwrap();
            ppu.fetch_bg_attribute(&cart).unwrap();
            ppu.fetch_bg_pattern_low(&cart).unwrap();
            ppu.fetch_bg_pattern_high(&cart).unwrap();

            assert_eq!(ppu.next_tile_id, 5);
            assert_eq!(ppu.next_tile_palette, 3); // 右下象限
            assert_eq!(ppu.next_tile_pattern_low, 0x96);
            assert_eq!(ppu.next_tile_pattern_high, 0x69);

            assert_eq!(ppu.v, old_v);
            assert_eq!(ppu.t, 0x1234);
            assert_eq!(ppu.fine_x, 5);
            assert_eq!(ppu.read_buffer, 0xA7);
            assert!(ppu.write_latch);

            // 改用图案表 0000，仍读取图块 5 的第 3 行。
            cart.ppu_write(0x0053, 0x12).unwrap();
            cart.ppu_write(0x005B, 0x34).unwrap();
            ppu.ctrl = 0;

            ppu.fetch_bg_pattern_low(&cart).unwrap();
            ppu.fetch_bg_pattern_high(&cart).unwrap();

            assert_eq!(ppu.next_tile_pattern_low, 0x12);
            assert_eq!(ppu.next_tile_pattern_high, 0x34);
        }

        #[test]
        fn fetches_all_four_attribute_quadrants() {
            let mut ppu = Ppu::new();
            let mut cart = cartridge();
            ppu.write(0x23C0, 0xE4, &mut cart).unwrap();

            for (x, y, expected) in [(0u16, 0u16, 0u8), (2, 0, 1), (0, 2, 2), (2, 2, 3)] {
                ppu.v = (y << 5) | x;
                ppu.fetch_bg_attribute(&cart).unwrap();
                assert_eq!(ppu.next_tile_palette, expected);
            }
        }

        #[test]
        fn loading_preserves_high_bytes_and_replaces_low_bytes() {
            let mut ppu = Ppu::new();

            for palette in 0..4u8 {
                ppu.bg_pattern_low = 0xAB12;
                ppu.bg_pattern_high = 0xCD34;
                ppu.bg_attribute_low = 0x55AA;
                ppu.bg_attribute_high = 0x66AA;

                ppu.next_tile_pattern_low = 0x96;
                ppu.next_tile_pattern_high = 0x69;
                ppu.next_tile_palette = palette;

                ppu.load_background_shifters();

                assert_eq!(ppu.bg_pattern_low, 0xAB96);
                assert_eq!(ppu.bg_pattern_high, 0xCD69);

                let low: u16 = if palette & 1 != 0 { 0xFF } else { 0 };
                let high: u16 = if palette & 2 != 0 { 0xFF } else { 0 };

                assert_eq!(ppu.bg_attribute_low, 0x5500 | low);
                assert_eq!(ppu.bg_attribute_high, 0x6600 | high);
            }
        }

        fn two_tiles() -> Ppu {
            let mut ppu = Ppu::new();

            // 第一个图块行：0,1,2,3,0,1,2,3，子调色板 1。
            ppu.next_tile_pattern_low = 0x55;
            ppu.next_tile_pattern_high = 0x33;
            ppu.next_tile_palette = 1;
            ppu.load_background_shifters();

            // 将第一个图块移到高字节。
            for _ in 0..8 {
                ppu.shift_background();
            }

            // 第二个图块行：3,2,1,0,3,2,1,0，子调色板 2。
            ppu.next_tile_pattern_low = 0xAA;
            ppu.next_tile_pattern_high = 0xCC;
            ppu.next_tile_palette = 2;
            ppu.load_background_shifters();

            ppu
        }

        #[test]
        fn outputs_two_tiles_with_matching_palettes() {
            let mut ppu = two_tiles();

            let expected = [0, 1, 2, 3, 0, 1, 2, 3, 3, 2, 1, 0, 3, 2, 1, 0];

            for (i, pixel) in expected.into_iter().enumerate() {
                let palette = if i < 8 { 1 } else { 2 };
                assert_eq!(ppu.background_pixel(), (pixel, palette));

                // 本测试约定先取像素，再移位。
                ppu.shift_background();
            }
        }

        #[test]
        fn fine_x_selects_pixels_across_a_tile_boundary() {
            let mut ppu = two_tiles();
            ppu.fine_x = 3;

            let expected = [
                (3, 1),
                (0, 1),
                (1, 1),
                (2, 1),
                (3, 1),
                (3, 2),
                (2, 2),
                (1, 2),
            ];

            for pair in expected {
                assert_eq!(ppu.background_pixel(), pair);

                // 读取本身不移动寄存器。
                assert_eq!(ppu.background_pixel(), pair);

                ppu.shift_background();
            }

            assert_eq!(ppu.fine_x, 3);
        }
    }

    #[cfg(test)]
    mod clocked_background_tests {
        use super::*;

        fn fixture(mask: u8, scroll_x: u8) -> (Ppu, Cartridge) {
            let mut bytes = vec![0u8; 16 + 16 * 1024];
            bytes[..4].copy_from_slice(b"NES\x1A");
            bytes[4] = 1;
            bytes[6] = 1; // 垂直镜像，两个横向页面独立。

            let mut cart = Cartridge::try_from(bytes.as_slice()).unwrap();
            let mut ppu = Ppu::new();

            for row in 0..8u16 {
                // 图块 1：每行、每列都有变化。
                let low = if row & 1 == 0 { 0x55 } else { 0xAA };
                let high = if row < 4 { 0x33 } else { 0xCC };

                cart.ppu_write(0x10 + row, low).unwrap();
                cart.ppu_write(0x18 + row, high).unwrap();

                // 图块 2：全部像素编号为 1。
                cart.ppu_write(0x20 + row, 0xFF).unwrap();
                cart.ppu_write(0x28 + row, 0).unwrap();
            }

            for page in 0..2u16 {
                let base = 0x2000 + page * 0x400;

                for ty in 0..30u16 {
                    for tx in 0..32u16 {
                        let tile = ((tx + ty + page) % 2 + 1) as u8;
                        ppu.write(base + ty * 32 + tx, tile, &mut cart).unwrap();
                    }
                }

                let attribute = if page == 0 { 0xE4 } else { 0x1B };
                for offset in 0..64u16 {
                    ppu.write(base + 0x3C0 + offset, attribute, &mut cart)
                        .unwrap();
                }
            }

            ppu.write(0x3F00, 0x0F, &mut cart).unwrap();
            for offset in 1..16u16 {
                ppu.write(0x3F00 + offset, 0x20 + offset as u8, &mut cart)
                    .unwrap();
            }

            ppu.cpu_write(0x2000, 0, &mut cart).unwrap();
            ppu.cpu_write(0x2005, scroll_x, &mut cart).unwrap();
            ppu.cpu_write(0x2005, 0, &mut cart).unwrap();
            ppu.cpu_write(0x2001, mask, &mut cart).unwrap();

            // 测试先经过预渲染，再绘制第一张完整背景。
            ppu.scanline = 261;
            ppu.dot = 0;

            (ppu, cart)
        }

        fn render_frame(ppu: &mut Ppu, cart: &Cartridge) {
            // 预渲染行 + 240 条可见行。
            for _ in 0..341 * 241 {
                ppu.clock(Some(cart)).unwrap();
            }

            assert_eq!(ppu.scanline, 240);
            assert_eq!(ppu.dot, 0);
        }

        fn expected_pixel(x: usize, y: usize, scroll_x: u8, mask: u8) -> u8 {
            let source_x = x + usize::from(scroll_x);
            let page = (source_x / 256) % 2;
            let tx = (source_x % 256) / 8;
            let ty = y / 8;
            let row = y % 8;

            let mut pixel = if (tx + ty + page) % 2 == 0 {
                // 图块 1。
                (source_x % 4) as u8 ^ (row & 1) as u8 ^ if row >= 4 { 2 } else { 0 }
            } else {
                // 图块 2。
                1
            };

            let mut palette = u8::from(tx & 2 != 0) | (u8::from(ty & 2 != 0) << 1);

            if page == 1 {
                palette = 3 - palette;
            }

            if mask & 0x08 == 0 || (x < 8 && mask & 0x02 == 0) {
                pixel = 0;
            }

            let color = if pixel == 0 {
                0x0F
            } else {
                0x20 + palette * 4 + pixel
            };

            color & if mask & 1 != 0 { 0x30 } else { 0x3F }
        }

        fn assert_frame(ppu: &Ppu, scroll_x: u8, mask: u8) {
            assert_eq!(ppu.framebuffer().len(), 256 * 240);

            for y in 0..240usize {
                for x in 0..256usize {
                    assert_eq!(
                        ppu.framebuffer()[y * 256 + x],
                        expected_pixel(x, y, scroll_x, mask),
                        "pixel ({x}, {y}), scroll={scroll_x}, mask={mask:02X}"
                    );
                }
            }
        }

        #[test]
        fn renders_complete_background_and_horizontal_scroll() {
            for scroll_x in [0, 3] {
                let (mut ppu, cart) = fixture(0x0A, scroll_x);
                render_frame(&mut ppu, &cart);
                assert_frame(&ppu, scroll_x, 0x0A);
            }
        }

        #[test]
        fn clips_left_eight_pixels_and_supports_greyscale() {
            for mask in [0x08, 0x0B] {
                let (mut ppu, cart) = fixture(mask, 0);
                render_frame(&mut ppu, &cart);
                assert_frame(&ppu, 0, mask);
            }
        }

        #[test]
        fn disabled_rendering_needs_no_cartridge_and_keeps_v() {
            let mut ppu = Ppu::new();
            ppu.v = 0x1234;
            ppu.t = 0x5678;
            ppu.mask = 0;
            ppu.palette_ram[0] = 0x0F;

            for _ in 0..341 {
                ppu.clock(None).unwrap();
            }

            assert_eq!(ppu.v, 0x1234);
            assert_eq!(ppu.t, 0x5678);
            assert_eq!(ppu.scanline, 1);
            assert_eq!(ppu.dot, 0);
            assert!(ppu.framebuffer()[..256].iter().all(|&v| v == 0x0F));
        }

        #[test]
        fn sprite_enable_alone_runs_background_fetch_pipeline() {
            let (mut ppu, cart) = fixture(0x10, 0);

            ppu.scanline = 0;
            ppu.dot = 1;
            ppu.v = 0;

            for _ in 0..8 {
                ppu.clock(Some(&cart)).unwrap();
            }

            // 背景不可见，但取数和地址递增仍运行。
            assert_eq!(ppu.v & 0x001F, 1);
            assert_eq!(ppu.next_tile_id, 1);
            assert!(ppu.framebuffer()[..8].iter().all(|&v| v == 0x0F));
        }

        #[test]
        fn clock_applies_scroll_updates_at_the_required_dots() {
            let (mut ppu, cart) = fixture(0x0A, 0);

            ppu.scanline = 10;
            ppu.dot = 256;
            ppu.v = 0;
            ppu.clock(Some(&cart)).unwrap();
            assert_eq!(ppu.v, 0x1001); // 横向 +1，fine Y +1。

            ppu.t = 0x0415;
            ppu.clock(Some(&cart)).unwrap(); // dot 257
            assert_eq!(ppu.v, 0x1415);

            ppu.scanline = 261;
            ppu.dot = 280;
            ppu.t = 0x32A0;
            ppu.clock(Some(&cart)).unwrap();
            assert_eq!(ppu.v, 0x36B5);

            // 验证不是只在 280 或 304 复制一次。
            ppu.t = 0x0840;
            ppu.clock(Some(&cart)).unwrap(); // dot 281
            assert_eq!(ppu.v, 0x0C55);
        }

        #[test]
        fn active_background_fetch_requires_a_cartridge() {
            let mut ppu = Ppu::new();
            ppu.mask = 0x08;
            ppu.scanline = 0;
            ppu.dot = 2;

            assert!(ppu.clock(None).is_err());
        }
    }

    #[cfg(test)]
    mod sprite_row_tests {
        use super::*;

        fn fixture() -> (Ppu, Cartridge) {
            // Mapper 0，16 KiB PRG，8 KiB CHR RAM。
            let mut rom = vec![0u8; 16 + 16 * 1024];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;

            let cartridge = Cartridge::try_from(rom.as_slice()).unwrap();

            let mut ppu = Ppu::new();
            // 隐藏所有未显式设置的精灵。
            ppu.oam.fill(0xFF);

            (ppu, cartridge)
        }

        fn set_sprite(ppu: &mut Ppu, index: usize, y: u8, tile: u8, attributes: u8, x: u8) {
            let start = index * 4;
            ppu.oam[start..start + 4].copy_from_slice(&[y, tile, attributes, x]);
        }

        fn write_row(cartridge: &mut Cartridge, address: u16, low: u8, high: u8) {
            cartridge.ppu_write(address, low).unwrap();
            cartridge.ppu_write(address + 8, high).unwrap();
        }

        #[test]
        fn sprite_pattern_addresses_cover_both_sizes_and_tables() {
            let (mut ppu, _) = fixture();

            // ctrl、tile_id、处理过翻转的 row、预期低位平面地址。
            let cases: [(u8, u8, u8, u16); 8] = [
                (0x00, 3, 2, 0x0032),
                (0x08, 3, 2, 0x1032),
                (0x20, 5, 0, 0x1040),
                (0x20, 5, 7, 0x1047),
                (0x20, 5, 8, 0x1050),
                (0x20, 5, 15, 0x1057),
                (0x20, 4, 0, 0x0040),
                // 8×16 模式忽略 ctrl bit 3。
                (0x28, 4, 0, 0x0040),
            ];

            for (ctrl, tile, row, expected) in cases {
                ppu.ctrl = ctrl;
                assert_eq!(
                    ppu.sprite_pattern_address(tile, row),
                    expected,
                    "ctrl={ctrl:#04X}, tile={tile}, row={row}"
                );
            }
        }

        #[test]
        fn selection_uses_y_plus_one_without_wrapping() {
            let (mut ppu, cartridge) = fixture();
            set_sprite(&mut ppu, 0, 20, 0, 0, 10);

            assert!(ppu.collect_sprite_rows(20, &cartridge).unwrap().is_empty());

            for scanline in 21..=28 {
                let rows = ppu.collect_sprite_rows(scanline, &cartridge).unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].oam_index, 0);
            }

            assert!(ppu.collect_sprite_rows(29, &cartridge).unwrap().is_empty());

            // Y=$FF 不能回绕后出现在第 0 行。
            ppu.oam.fill(0xFF);
            assert!(ppu.collect_sprite_rows(0, &cartridge).unwrap().is_empty());

            // 非可见扫描线不准备输出。
            set_sprite(&mut ppu, 0, 239, 0, 0, 0);
            assert!(ppu.collect_sprite_rows(240, &cartridge).unwrap().is_empty());
        }

        #[test]
        fn first_eight_in_oam_order_win_even_when_transparent() {
            let (mut ppu, mut cartridge) = fixture();

            // Tile 0 全透明；tile 1 的首行全部为像素 1。
            write_row(&mut cartridge, 0x0010, 0xFF, 0);

            // OAM 0 隐藏。OAM 1～9 都覆盖扫描线 1。
            for index in 1..=9 {
                let tile = if index == 9 { 1 } else { 0 };
                set_sprite(&mut ppu, index, 0, tile, 0, 255);
            }

            let rows = ppu.collect_sprite_rows(1, &cartridge).unwrap();

            assert_eq!(rows.len(), 8);
            let indices: Vec<u8> = rows.iter().map(|row| row.oam_index).collect();
            assert_eq!(indices, vec![1, 2, 3, 4, 5, 6, 7, 8]);

            assert!(rows.iter().all(|row| row.pixels == [0; 8]));
            assert!(rows.iter().all(|row| row.x == 255));
        }

        #[test]
        fn eight_by_eight_pixels_and_all_flip_combinations() {
            let (mut ppu, mut cartridge) = fixture();
            ppu.ctrl = 0x08; // 精灵图案表 $1000。

            // Tile 3 第 0 行：1,2,0,0,0,0,0,0。
            write_row(&mut cartridge, 0x1030, 0x80, 0x40);
            // Tile 3 第 7 行：0,0,0,0,0,0,2,1。
            write_row(&mut cartridge, 0x1037, 0x01, 0x02);

            let cases: [(u8, [u8; 8]); 4] = [
                (0x00, [1, 2, 0, 0, 0, 0, 0, 0]),
                (0x40, [0, 0, 0, 0, 0, 0, 2, 1]),
                (0x80, [0, 0, 0, 0, 0, 0, 2, 1]),
                (0xC0, [1, 2, 0, 0, 0, 0, 0, 0]),
            ];

            for (flip, expected) in cases {
                // $23：子调色板 3，位于背景后方。
                set_sprite(&mut ppu, 0, 0, 3, 0x23 | flip, 37);

                let rows = ppu.collect_sprite_rows(1, &cartridge).unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].pixels, expected, "flip={flip:#04X}");
                assert_eq!(rows[0].palette, 3);
                assert!(rows[0].behind_background);
                assert_eq!(rows[0].x, 37);
                assert_eq!(rows[0].oam_index, 0);
            }

            set_sprite(&mut ppu, 0, 0, 3, 0, 37);
            let rows = ppu.collect_sprite_rows(1, &cartridge).unwrap();
            assert!(!rows[0].behind_background);
        }

        #[test]
        fn eight_by_sixteen_vertical_flip_swaps_the_two_tiles() {
            let (mut ppu, mut cartridge) = fixture();
            ppu.ctrl = 0x20;

            // tile_id=5：表 $1000，上半图块 4，下半图块 5。
            write_row(&mut cartridge, 0x1040, 0xFF, 0); // 行 0：1
            write_row(&mut cartridge, 0x1047, 0, 0xFF); // 行 7：2
            write_row(&mut cartridge, 0x1050, 0xFF, 0xFF); // 行 8：3
            write_row(&mut cartridge, 0x1057, 0, 0); // 行15：0

            let cases: [(u8, u16, u8); 8] = [
                (0x00, 1, 1),
                (0x00, 8, 2),
                (0x00, 9, 3),
                (0x00, 16, 0),
                (0x80, 1, 0),
                (0x80, 8, 3),
                (0x80, 9, 2),
                (0x80, 16, 1),
            ];

            for (attributes, scanline, pixel) in cases {
                set_sprite(&mut ppu, 0, 0, 5, attributes, 0);
                let rows = ppu.collect_sprite_rows(scanline, &cartridge).unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(
                    rows[0].pixels, [pixel; 8],
                    "attributes={attributes:#04X}, scanline={scanline}"
                );
            }

            assert!(ppu.collect_sprite_rows(17, &cartridge).unwrap().is_empty());
        }
    }

    #[cfg(test)]
    mod sprite_composition_tests {
        use super::*;

        fn row(index: u8, x: u8, pixel: u8, palette: u8, behind: bool) -> SpriteRow {
            SpriteRow {
                oam_index: index,
                x,
                palette,
                behind_background: behind,
                pixels: [pixel; 8],
            }
        }

        #[test]
        fn sprite_selection_skips_transparent_pixels_and_clips_right_edge() {
            let mut ppu = Ppu::new();

            ppu.sprite_rows = vec![
                row(2, 40, 0, 1, false),
                row(5, 40, 3, 2, true),
                row(6, 40, 1, 0, false),
            ];

            assert_eq!(ppu.sprite_pixel(39), None);
            assert_eq!(
                ppu.sprite_pixel(42),
                Some(SpritePixel {
                    pixel: 3,
                    palette: 2,
                    behind_background: true,
                    is_sprite_zero: false,
                })
            );
            assert_eq!(ppu.sprite_pixel(48), None);

            // 每个 X 必须重新选择：前一个精灵这一像素不再透明。
            ppu.sprite_rows[0].pixels[2] = 1;
            assert_eq!(ppu.sprite_pixel(42).unwrap().palette, 1);

            ppu.sprite_rows = vec![row(0, 254, 2, 0, false)];
            assert!(ppu.sprite_pixel(254).is_some());
            assert!(ppu.sprite_pixel(255).is_some());
            assert_eq!(ppu.sprite_pixel(256), None);
            assert_eq!(ppu.sprite_pixel(0), None);
        }

        #[test]
        fn composition_obeys_transparency_and_background_priority() {
            let mut ppu = Ppu::new();
            ppu.mask = 0x1E; // 背景、精灵及其左侧显示全部启用。

            // 使用非零 OAM 序号，避免本测试夹带 sprite 0 hit。
            ppu.sprite_rows = vec![row(2, 40, 3, 2, false)];

            // 两者不透明，精灵在前。
            assert_eq!(ppu.compose_pixel(42, 2, 1), 0x3F1B);

            // 精灵在后，显示背景。
            ppu.sprite_rows[0].behind_background = true;
            assert_eq!(ppu.compose_pixel(42, 2, 1), 0x3F06);

            // 背景透明时，后方精灵仍然显示。
            assert_eq!(ppu.compose_pixel(42, 0, 1), 0x3F1B);

            // 精灵透明时显示背景；两者透明则显示通用背景色。
            ppu.sprite_rows[0].pixels = [0; 8];
            assert_eq!(ppu.compose_pixel(42, 2, 1), 0x3F06);
            assert_eq!(ppu.compose_pixel(42, 0, 1), 0x3F00);
        }

        #[test]
        fn hidden_frontmost_sprite_does_not_reveal_a_later_sprite() {
            let mut ppu = Ppu::new();
            ppu.mask = 0x1E;
            ppu.sprite_rows = vec![row(2, 40, 3, 2, true), row(5, 40, 1, 0, false)];

            // 选中 OAM 2 后与背景比较，不能改选 OAM 5。
            assert_eq!(ppu.compose_pixel(42, 2, 1), 0x3F06);
        }

        #[test]
        fn masks_control_both_output_and_sprite_zero_hit() {
            // mask、X、最终调色板地址、是否命中。
            let cases: [(u8, u16, u16, bool); 8] = [
                (0x1E, 0, 0x3F1B, true),
                (0x1C, 0, 0x3F1B, false), // 左侧背景隐藏。
                (0x1A, 0, 0x3F06, false), // 左侧精灵隐藏。
                (0x18, 0, 0x3F00, false), // 左侧两者都隐藏。
                (0x18, 8, 0x3F1B, true),  // X=8 已不受左侧裁剪。
                (0x14, 8, 0x3F1B, false), // 背景关闭。
                (0x0A, 8, 0x3F06, false), // 精灵关闭。
                (0x00, 8, 0x3F00, false), // 两者关闭。
            ];

            for (mask, x, expected_addr, hit) in cases {
                let mut ppu = Ppu::new();
                ppu.mask = mask;
                ppu.sprite_rows = vec![row(0, x as u8, 3, 2, false)];

                assert_eq!(
                    ppu.compose_pixel(x, 2, 1),
                    expected_addr,
                    "mask={mask:#04X}, x={x}"
                );
                assert_eq!(ppu.status & 0x40 != 0, hit, "mask={mask:#04X}, x={x}");
            }
        }

        #[test]
        fn sprite_zero_hit_ignores_priority_and_remains_set() {
            let mut ppu = Ppu::new();
            ppu.mask = 0x1E;
            ppu.sprite_rows = vec![row(0, 40, 3, 2, true)];

            // 精灵在背景后面也能命中。
            assert_eq!(ppu.compose_pixel(42, 2, 1), 0x3F06);
            assert_ne!(ppu.status & 0x40, 0);

            // 下一个像素没有重叠，也不能清除之前的命中。
            ppu.compose_pixel(100, 0, 0);
            assert_ne!(ppu.status & 0x40, 0);

            // 预渲染行 dot 1 清除。
            ppu.scanline = 261;
            ppu.dot = 1;
            ppu.clock_test();
            assert_eq!(ppu.status & 0x40, 0);
        }

        #[test]
        fn sprite_zero_hit_rejects_transparency_wrong_index_and_x255() {
            // OAM 序号、精灵像素、背景像素、X。
            let cases: [(u8, u8, u8, u16); 4] =
                [(0, 0, 2, 40), (0, 3, 0, 40), (7, 3, 2, 40), (0, 3, 2, 255)];

            for (index, sprite, background, x) in cases {
                let mut ppu = Ppu::new();
                ppu.mask = 0x1E;
                ppu.sprite_rows = vec![row(index, x as u8, sprite, 2, false)];

                ppu.compose_pixel(x, background, 1);
                assert_eq!(ppu.status & 0x40, 0);
            }
        }

        #[test]
        fn clock_prepares_next_line_and_writes_sprite_colors() {
            let mut rom = vec![0u8; 16 + 16 * 1024];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;
            let mut cartridge = Cartridge::try_from(rom.as_slice()).unwrap();

            // Tile 2 首行：8 个像素全部为 1。
            cartridge.ppu_write(0x0020, 0xFF).unwrap();
            cartridge.ppu_write(0x0028, 0x00).unwrap();

            let mut ppu = Ppu::new();
            ppu.oam.fill(0xFF);
            // Y=0 → 屏幕第 1 行；tile=2；palette=0；X=10。
            ppu.oam[..4].copy_from_slice(&[0, 2, 0, 10]);
            ppu.mask = 0x14; // 只显示精灵，包括左侧。
            ppu.palette_ram[0] = 0x0F;
            ppu.palette_ram[0x11] = 0x2A;

            ppu.scanline = 0;
            ppu.dot = 257;

            // 到第 1 行 dot 19，已绘制该行 X=0～17。
            for _ in 0..103 {
                ppu.clock(Some(&cartridge)).unwrap();
            }

            assert_eq!((ppu.scanline, ppu.dot), (1, 19));
            assert_eq!(ppu.sprite_rows.len(), 1);

            for x in 0..18 {
                let expected = if x >= 10 { 0x2A } else { 0x0F };
                assert_eq!(ppu.framebuffer[256 + x], expected, "x={x}");
            }

            // X=18 已在精灵之外。
            ppu.clock(Some(&cartridge)).unwrap();
            assert_eq!(ppu.framebuffer[256 + 18], 0x0F);

            // 第 0 行不能复用上一帧残留的精灵。
            ppu.scanline = 261;
            ppu.dot = 257;
            ppu.next_sprite_rows = vec![row(0, 10, 1, 0, false)];

            // 84 个时钟到第 0 行 dot 0，再一个时钟完成切换。
            for _ in 0..85 {
                ppu.clock(Some(&cartridge)).unwrap();
            }

            assert_eq!((ppu.scanline, ppu.dot), (0, 1));
            assert!(ppu.sprite_rows.is_empty());
        }
    }
}
