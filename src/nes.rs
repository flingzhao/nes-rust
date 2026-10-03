use crate::controller::Button;
use crate::{bus::Bus, cpu::Cpu};

pub struct Nes {
    cpu: Cpu,
    bus: Bus,
    cpu_cycles: u64,
}

impl Nes {
    pub fn new(cpu: Cpu, bus: Bus) -> Self {
        Self {
            cpu,
            bus,
            cpu_cycles: 0,
        }
    }

    pub fn step(&mut self) -> Result<u8, String> {
        self.cpu.set_nmi_line(self.bus.ppu().nmi_line());
        let cycle = self.cpu.step(&mut self.bus)?;
        for _ in 0..cycle {
            self.clock_one_cpu_cycle()?;
        }

        self.run_pending_oam_dma()?;

        Ok(cycle)
    }

    pub fn cpu_cycles(&self) -> u64 {
        self.cpu_cycles
    }

    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    fn run_pending_oam_dma(&mut self) -> Result<(), String> {
        let Some(page) = self.bus.take_oam_dma_request() else {
            return Ok(());
        };

        let needs_alignment = self.cpu_cycles & 1 != 0;

        // 暂停周期。
        self.clock_one_cpu_cycle()?;

        // 必要时增加一个对齐周期。
        if needs_alignment {
            self.clock_one_cpu_cycle()?;
        }

        for offset in 0..256u16 {
            let source_base = u16::from(page) << 8;

            let value = self.bus.cpu_read(source_base | offset)?;
            self.clock_one_cpu_cycle()?;

            self.bus.cpu_write(0x2004, value)?;
            self.clock_one_cpu_cycle()?;
        }
        Ok(())
    }

    fn clock_one_cpu_cycle(&mut self) -> Result<(), String> {
        let levels = self.bus.clock_cpu_cycle()?;

        for level in levels {
            self.cpu.set_nmi_line(level);
        }

        self.cpu_cycles = self.cpu_cycles.wrapping_add(1);
        Ok(())
    }

    pub fn framebuffer(&self) -> &[u8] {
        self.bus.ppu().framebuffer()
    }

    pub fn run_until_frame(&mut self) -> Result<(), String> {
        if self.bus.take_frame_ready() {
            return Ok(());
        }
        for _ in 0..100_000 {
            self.step()?;
            if self.bus.take_frame_ready() {
                return Ok(());
            }
        }
        Err("PPU frame was not ready after 100000 CPU steps".to_string())
    }

    pub fn set_controller_button(
        &mut self,
        port: usize,
        button: Button,
        pressed: bool,
    ) -> Result<(), String> {
        self.bus.set_controller_button(port, button, pressed)
    }

    pub fn release_all_buttons(&mut self) {
        self.bus.release_all_buttons();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::Cartridge;

    fn nes_with_program(program: &[u8]) -> Nes {
        assert!(program.len() <= 0x3FFA);

        let mut rom = vec![0u8; 16 + 0x4000];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1; // 16 KiB PRG，Mapper 0，CHR RAM。

        rom[16..16 + program.len()].copy_from_slice(program);

        // Reset 向量指向 $8000。
        rom[16 + 0x3FFC..16 + 0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());

        let cartridge = Cartridge::try_from(rom.as_slice()).unwrap();
        let mut bus = Bus::new();
        bus.insert_cartridge(cartridge);

        let mut cpu = Cpu::new();
        cpu.reset(&mut bus).unwrap();

        Nes::new(cpu, bus)
    }

    #[test]
    fn system_timing_advances_three_ppu_ticks_per_cpu_cycle() {
        // LDA #$01; INX; NOP，分别为 2 个 CPU 周期。
        let mut nes = nes_with_program(&[0xA9, 0x01, 0xE8, 0xEA]);

        for instruction_count in 1u64..=3 {
            assert_eq!(nes.step().unwrap(), 2);

            assert_eq!(nes.cpu_cycles(), instruction_count * 2);
            assert_eq!(nes.bus().ppu().dot(), (instruction_count * 6) as u16);
            assert_eq!(nes.bus().ppu().scanline(), 0);
            assert_eq!(nes.bus().ppu().frame(), 0);
        }
    }

    #[test]
    fn system_timing_preserves_ticks_across_frame_boundary() {
        // JMP $8000：每次 3 个 CPU 周期，不断循环。
        let mut nes = nes_with_program(&[0x4C, 0x00, 0x80]);

        for _ in 0..10_000 {
            assert_eq!(nes.step().unwrap(), 3);
        }

        assert_eq!(nes.cpu_cycles(), 30_000);

        // 90000 PPU 周期 = 1 帧 + 658 周期
        // 658 = 1 条扫描线 + 317 周期。
        assert_eq!(nes.bus().ppu().frame(), 1);
        assert_eq!(nes.bus().ppu().scanline(), 1);
        assert_eq!(nes.bus().ppu().dot(), 317);
    }

    #[test]
    fn system_timing_counts_brk_entry_cycles() {
        // 测试 ROM 的 IRQ 向量默认指向 $0000；
        // 本测试只执行 BRK 入口，不执行处理程序。
        let mut nes = nes_with_program(&[0x00, 0xEA]);

        assert_eq!(nes.step().unwrap(), 7);
        assert_eq!(nes.cpu_cycles(), 7);
        assert_eq!(nes.bus().ppu().dot(), 21);
    }

    #[test]
    fn system_vblank_drives_cpu_nmi_once_per_edge() {
        use crate::cartridge::Cartridge;

        let mut rom = vec![0u8; 16 + 0x4000];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;

        let prg = &mut rom[16..];
        prg[..3].copy_from_slice(&[0x4C, 0x00, 0x80]); // JMP $8000
        prg[0x1000] = 0x40; // $9000: RTI

        prg[0x3FFA..0x3FFC].copy_from_slice(&0x9000u16.to_le_bytes());
        prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());

        let cart = Cartridge::try_from(rom.as_slice()).unwrap();
        let mut bus = Bus::new();
        bus.insert_cartridge(cart);
        bus.cpu_write(0x2000, 0x80).unwrap();

        let mut cpu = Cpu::new();
        cpu.reset(&mut bus).unwrap();
        let mut nes = Nes::new(cpu, bus);

        // 每条 JMP 为 3 CPU 周期，即 9 PPU ticks。
        // 第 9132 条 JMP 推进期间跨过 VBlank 置位时刻。
        for _ in 0..9132 {
            assert_eq!(nes.step().unwrap(), 3);
        }

        assert!(nes.bus().ppu().nmi_line());

        let before = nes.cpu_cycles();

        assert_eq!(nes.step().unwrap(), 7); // NMI 入口
        assert_eq!(nes.cpu_cycles(), before + 7);

        // 压栈返回地址应为 $8000。
        assert_eq!(nes.bus.cpu_read(0x01FD).unwrap(), 0x80);
        assert_eq!(nes.bus.cpu_read(0x01FC).unwrap(), 0x00);

        assert_eq!(nes.step().unwrap(), 6); // RTI

        // 信号持续有效，但没有新边沿，不能不断重入。
        assert!(nes.bus().ppu().nmi_line());
        assert_eq!(nes.step().unwrap(), 3); // JMP $8000
        assert_eq!(nes.step().unwrap(), 3);
    }

    #[cfg(test)]
    mod oam_dma_tests {
        use super::*;
        // 根据项目结构导入 Cpu、Bus、Cartridge。

        fn fixture() -> Nes {
            let mut bytes = vec![0u8; 16 + 16 * 1024];
            bytes[..4].copy_from_slice(b"NES\x1A");
            bytes[4] = 1;

            // $8000：
            // LDA #$02
            // STA $4014
            // NOP
            bytes[16..22].copy_from_slice(&[0xA9, 0x02, 0x8D, 0x14, 0x40, 0xEA]);

            // Mapper 0，16 KiB PRG 的复位向量。
            bytes[16 + 0x3FFC] = 0x00;
            bytes[16 + 0x3FFD] = 0x80;

            let cart = Cartridge::try_from(bytes.as_slice()).unwrap();
            let mut bus = Bus::new();
            bus.insert_cartridge(cart);

            let mut cpu = Cpu::new();
            cpu.reset(&mut bus).unwrap();

            Nes::new(cpu, bus)
        }

        fn fill_page(nes: &mut Nes, page: u8, seed: u8) {
            for offset in 0..256u16 {
                let addr = (u16::from(page) << 8) | offset;
                let value = (offset as u8) ^ seed;
                nes.bus.cpu_write(addr, value).unwrap();
            }
        }

        fn expected_oam_byte(destination: u8, value: u8) -> u8 {
            if destination & 3 == 2 {
                value & 0xE3
            } else {
                value
            }
        }

        #[test]
        fn oam_data_writes_increment_and_reads_do_not() {
            let mut nes = fixture();

            nes.bus.cpu_write(0x2003, 0xFF).unwrap();
            nes.bus.cpu_write(0x2004, 0x12).unwrap();
            nes.bus.cpu_write(0x2004, 0x34).unwrap();

            assert_eq!(nes.bus.ppu().oam()[255], 0x12);
            assert_eq!(nes.bus.ppu().oam()[0], 0x34);

            nes.bus.cpu_write(0x2003, 0xFF).unwrap();

            assert_eq!(nes.bus.cpu_read(0x2004).unwrap(), 0x12);
            assert_eq!(nes.bus.cpu_read(0x2004).unwrap(), 0x12);

            // 两次读没有递增，下一次写仍写到 FF。
            nes.bus.cpu_write(0x2004, 0x56).unwrap();
            assert_eq!(nes.bus.ppu().oam()[255], 0x56);
        }

        #[test]
        fn oam_attribute_unused_bits_read_as_zero() {
            let mut nes = fixture();

            nes.bus.cpu_write(0x2003, 2).unwrap();
            nes.bus.cpu_write(0x2004, 0xFF).unwrap();

            nes.bus.cpu_write(0x2003, 2).unwrap();
            assert_eq!(nes.bus.cpu_read(0x2004).unwrap(), 0xE3);

            // 普通字节不应用属性掩码。
            nes.bus.cpu_write(0x2003, 3).unwrap();
            nes.bus.cpu_write(0x2004, 0xFF).unwrap();

            nes.bus.cpu_write(0x2003, 3).unwrap();
            assert_eq!(nes.bus.cpu_read(0x2004).unwrap(), 0xFF);
        }

        #[test]
        fn dma_copies_a_page_and_wraps_from_current_oam_address() {
            let mut nes = fixture();
            fill_page(&mut nes, 2, 0xA5);

            let start = 0xFAu8;
            nes.bus.cpu_write(0x2003, start).unwrap();
            nes.bus.cpu_write(0x4014, 2).unwrap();

            nes.run_pending_oam_dma().unwrap();

            assert_eq!(nes.cpu_cycles(), 513);

            for offset in 0..256u16 {
                let destination = start.wrapping_add(offset as u8);
                let source = (offset as u8) ^ 0xA5;

                assert_eq!(
                    nes.bus.ppu().oam()[usize::from(destination)],
                    expected_oam_byte(destination, source),
                    "source offset={offset}, destination={destination}"
                );
            }

            // 写入 256 字节后，OAMADDR 回到起始位置。
            nes.bus.cpu_write(0x2004, 0x77).unwrap();

            assert_eq!(
                nes.bus.ppu().oam()[usize::from(start)],
                expected_oam_byte(start, 0x77)
            );

            // 已消费的请求不能重复执行。
            let before = nes.cpu_cycles();
            nes.run_pending_oam_dma().unwrap();
            assert_eq!(nes.cpu_cycles(), before);
        }

        #[test]
        fn dma_alignment_depends_on_starting_cycle_parity() {
            for initial_cycles in [0u64, 1] {
                let mut nes = fixture();

                if initial_cycles == 1 {
                    nes.clock_one_cpu_cycle().unwrap();
                }

                nes.bus.cpu_write(0x4014, 2).unwrap();
                let before = nes.cpu_cycles();

                nes.run_pending_oam_dma().unwrap();

                let expected = if initial_cycles == 0 { 513 } else { 514 };
                assert_eq!(nes.cpu_cycles() - before, expected);
            }
        }

        #[test]
        fn latest_dma_request_wins_and_cpu_ram_mirrors_are_used() {
            let mut nes = fixture();
            fill_page(&mut nes, 2, 0xA5);
            fill_page(&mut nes, 3, 0x5A);

            nes.bus.cpu_write(0x4014, 2).unwrap();

            // $0B00 镜像到 $0300，且覆盖前一个请求。
            nes.bus.cpu_write(0x4014, 0x0B).unwrap();
            nes.run_pending_oam_dma().unwrap();

            for offset in 0..256u16 {
                let destination = offset as u8;
                let source = destination ^ 0x5A;

                assert_eq!(
                    nes.bus.ppu().oam()[usize::from(destination)],
                    expected_oam_byte(destination, source)
                );
            }
        }

        #[test]
        fn step_runs_dma_before_executing_the_next_instruction() {
            let mut nes = fixture();
            fill_page(&mut nes, 2, 0xA5);

            assert_eq!(nes.step().unwrap(), 2); // LDA
            assert_eq!(nes.cpu_cycles(), 2);

            assert_eq!(nes.step().unwrap(), 4); // STA + DMA
            assert_eq!(nes.cpu_cycles(), 2 + 4 + 513);

            assert_eq!(nes.bus.ppu().oam()[0], 0xA5);
            assert_eq!(nes.bus.ppu().oam()[255], 0xFF ^ 0xA5);

            assert_eq!(nes.step().unwrap(), 2); // DMA 后才执行 NOP
            assert_eq!(nes.cpu_cycles(), 2 + 4 + 513 + 2);
        }
    }

    #[cfg(test)]
    mod frame_runner_tests {
        use super::*;

        fn looping_nes() -> Nes {
            let mut rom = vec![0u8; 16 + 16 * 1024];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;

            // $8000: JMP $8000
            rom[16..19].copy_from_slice(&[0x4C, 0x00, 0x80]);

            // 16 KiB PRG 镜像下的 reset vector。
            rom[16 + 0x3FFC] = 0x00;
            rom[16 + 0x3FFD] = 0x80;

            let cartridge = Cartridge::try_from(rom.as_slice()).unwrap();
            let mut bus = Bus::new();
            bus.insert_cartridge(cartridge);

            let mut cpu = Cpu::new();
            cpu.reset(&mut bus).unwrap();

            Nes::new(cpu, bus)
        }

        #[test]
        fn runs_cpu_and_consumes_each_frame_notification() {
            let mut nes = looping_nes();

            let before = nes.cpu_cycles();
            nes.run_until_frame().unwrap();

            assert!(nes.cpu_cycles() > before);
            assert_eq!(nes.framebuffer().len(), 256 * 240);
            assert!(nes.framebuffer().iter().all(|&color| color < 64));
            assert!(!nes.bus.take_frame_ready());

            // 第二次不能直接返回上一帧的通知。
            let before = nes.cpu_cycles();
            nes.run_until_frame().unwrap();
            let elapsed = nes.cpu_cycles() - before;

            // 两次画面完成通知之间约为 29781 个 CPU 周期。
            // 不写死精确值，允许指令边界造成的小偏差。
            assert!(
                (29_000..31_000).contains(&elapsed),
                "unexpected CPU cycles between frames: {elapsed}"
            );
            assert!(!nes.bus.take_frame_ready());
        }
    }
}
