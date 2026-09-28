pub struct Ppu {
    dot: u16,
    scanline: u16,
    frame: u64,
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            dot: 0,
            scanline: 0,
            frame: 0,
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
}
