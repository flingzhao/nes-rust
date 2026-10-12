use crate::controller::Button;
use crate::{bus::Bus, cpu::Cpu, cpu_bus::CpuBus};
use crate::{dmc_dma::CpuBusCycle, dmc_dma::DmaPhase, dmc_dma::DmcDma, dmc_dma::DmcDmaAction};
use crate::{oam_dma::OamDma, oam_dma::OamDmaAction};

pub struct Nes {
    cpu: Cpu,
    bus: Bus,
    cpu_cycles: u64,
    dmc_dma: DmcDma,
}

impl Nes {
    pub fn new(cpu: Cpu, bus: Bus) -> Self {
        Self {
            cpu,
            bus,
            cpu_cycles: 0,
            dmc_dma: DmcDma::new(),
        }
    }

    pub fn step(&mut self) -> Result<u8, String> {
        self.cpu.set_nmi_line(self.bus.ppu().nmi_line());
        self.cpu.set_irq_line(self.bus.irq_line());

        let (result, nmi_levels) = {
            let mut bus =
                ClockedCpuBus::new(&mut self.bus, &mut self.cpu_cycles, &mut self.dmc_dma);

            let result = self.cpu.step(&mut bus);
            let nmi_levels = bus.take_nmi_levels();

            (result, nmi_levels)
        };

        // 适配器的借用已结束，现在可以更新 CPU 的信号。
        for level in nmi_levels {
            self.cpu.set_nmi_line(level);
        }
        self.cpu.set_irq_line(self.bus.irq_line());

        let cycles = result?;

        // self.run_pending_oam_dma()?;

        Ok(cycles)
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
        self.cpu.set_nmi_line(self.bus.ppu().nmi_line());

        let levels = self.bus.clock_cpu_cycle()?;
        // 在当前apu运行一个cpu周期后，更新irq
        self.cpu.set_irq_line(self.bus.irq_line());

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

    pub fn take_audio_samples(&mut self) -> Vec<f32> {
        self.bus.take_audio_samples()
    }
}

struct ClockedCpuBus<'a> {
    bus: &'a mut Bus,
    cpu_cycles: &'a mut u64,
    dmc_dma: &'a mut DmcDma,
    nmi_levels: Vec<bool>,
}

impl<'a> ClockedCpuBus<'a> {
    fn new(bus: &'a mut Bus, cpu_cycles: &'a mut u64, dmc_dma: &'a mut DmcDma) -> Self {
        Self {
            bus,
            cpu_cycles,
            dmc_dma,
            nmi_levels: Vec::new(),
        }
    }

    fn clock_one_cpu_cycle(&mut self) -> Result<(), String> {
        let nmi = self.bus.ppu().nmi_line();
        self.nmi_levels.push(nmi);

        let nmi_levels = self.bus.clock_cpu_cycle()?;

        self.nmi_levels.extend(nmi_levels);

        self.schedule_dmc_reload()?;

        *self.cpu_cycles = (*self.cpu_cycles).wrapping_add(1);
        Ok(())
    }

    fn take_nmi_levels(&mut self) -> Vec<bool> {
        std::mem::take(&mut self.nmi_levels)
    }

    fn schedule_dmc_reload(&mut self) -> Result<(), String> {
        if self.dmc_dma.is_busy() {
            return Ok(());
        }

        if let Some(sample_addr) = self.bus.dmc_next_sample_address() {
            self.dmc_dma.request_reload(sample_addr)?;
        }

        Ok(())
    }
}

impl CpuBus for ClockedCpuBus<'_> {
    fn cpu_read(&mut self, addr: u16) -> Result<u8, String> {
        let mut oam_dma = OamDma::new();

        if let Some(page) = self.bus.take_oam_dma_request() {
            oam_dma.request(page)?;
        }
        loop {
            let phase = if *self.cpu_cycles & 1 == 0 {
                DmaPhase::Put
            } else {
                DmaPhase::Get
            };
            let dmc_action = self.dmc_dma.clock(phase, CpuBusCycle::Read);
            if let DmcDmaAction::Read(sample_addr) = dmc_action {
                let sample_value = self.bus.cpu_read(sample_addr)?;
                self.bus.apu().complete_dmc_sample_read(sample_value)?;
                self.clock_one_cpu_cycle()?;
                continue;
            }
            let oam_action = oam_dma.clock(phase, CpuBusCycle::Read)?;
            match oam_action {
                OamDmaAction::Read(source_addr) => {
                    let value = self.bus.cpu_read(source_addr)?;
                    oam_dma.complete_read(value)?;
                    self.clock_one_cpu_cycle()?;
                    continue;
                }
                OamDmaAction::Write(value) => {
                    self.bus.cpu_write(0x2004, value)?;
                    self.clock_one_cpu_cycle()?;
                    continue;
                }
                OamDmaAction::Pass => {
                    if dmc_action == DmcDmaAction::Pass {
                        let value = self.bus.cpu_read(addr)?;
                        self.clock_one_cpu_cycle()?;
                        return Ok(value);
                    } else {
                        let _ = self.bus.cpu_read(addr)?;
                        self.clock_one_cpu_cycle()?;
                        continue;
                    }
                }
                _ => {
                    let _ = self.bus.cpu_read(addr)?;
                    self.clock_one_cpu_cycle()?;
                    continue;
                }
            }
        }
    }

    fn cpu_write(&mut self, addr: u16, value: u8) -> Result<(), String> {
        let phase = if *self.cpu_cycles & 1 == 0 {
            DmaPhase::Put
        } else {
            DmaPhase::Get
        };
        let action = self.dmc_dma.clock(phase, CpuBusCycle::Write);
        match action {
            DmcDmaAction::Pass => {
                let former_dmc_active = self.bus.dmc_active();
                self.bus.cpu_write(addr, value)?;

                if addr == 0x4015 && value & 0x10 == 0 {
                    self.dmc_dma.cancel_pending()?;
                }

                if addr == 0x4015
                    && value & 0x10 != 0
                    && !former_dmc_active
                    && !self.dmc_dma.is_busy()
                {
                    if let Some(sample_addr) = self.bus.dmc_next_sample_address() {
                        self.dmc_dma.request_load(sample_addr, phase)?;
                    }
                }
                self.clock_one_cpu_cycle()?;
            }
            _ => unreachable!(),
        }
        Ok(())
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

    // 将本文件整个追加到 /home/hgg/src/nes/src/nes.rs 的末尾。
    // 不替换已有 tests 模块。要求新增 Apu::irq_line()、Bus::irq_line()。
    // 这些测试针对本轮“按 CPU 周期推进、顺序访问寄存器”的模型；
    // 不验证真实硬件同周期读 $4015 与 IRQ 置位的相位竞争。
    #[cfg(test)]
    mod apu_frame_irq_tests {
        use super::*;
        use crate::apu::Apu;
        use crate::cartridge::Cartridge;

        fn advance(apu: &mut Apu, cycles: usize) {
            for _ in 0..cycles {
                apu.clock_cpu_cycle();
            }
        }

        #[test]
        fn irq_first_asserts_at_29828_and_survives_sequence_wrap() {
            let mut apu = Apu::new();
            advance(&mut apu, 29_827);
            assert!(!apu.irq_line());
            apu.clock_cpu_cycle();
            assert!(apu.irq_line());
            advance(&mut apu, 100);
            assert!(apu.irq_line(), "序列归零不能自动清除已挂起的 IRQ");
            assert_eq!(apu.read_status() & 0x40, 0x40);
            assert!(!apu.irq_line());
            assert_eq!(apu.read_status() & 0x40, 0);
        }

        #[test]
        fn terminal_cycles_can_reassert_irq_independently_of_block_ticks() {
            let mut apu = Apu::new();
            advance(&mut apu, 29_827);
            for cycle in 29_828..=29_830 {
                apu.clock_cpu_cycle();
                assert!(apu.irq_line(), "cycle={cycle}");
                // 本轮顺序调用约定：本次推进完成后，读取并确认该请求。
                assert_eq!(apu.read_status() & 0x40, 0x40);
                assert!(!apu.irq_line());
            }
            apu.clock_cpu_cycle();
            assert!(!apu.irq_line());
        }

        #[test]
        fn status_returns_old_irq_bit_without_clearing_channel_activity() {
            let mut apu = Apu::new();
            apu.write_register(0x4015, 3);
            for base in [0x4000u16, 0x4004] {
                apu.write_register(base, 0x3F); // 固定音量15，暂停长度递减。
                apu.write_register(base + 2, 8);
                apu.write_register(base + 3, 0); // 长度10。
            }
            advance(&mut apu, 29_831);
            assert_eq!(apu.read_status(), 0x43);
            assert_eq!(apu.read_status(), 0x03);
            assert!(!apu.irq_line());
        }

        #[test]
        fn writing_4015_does_not_acknowledge_frame_irq() {
            let mut apu = Apu::new();
            advance(&mut apu, 29_831);
            apu.write_register(0x4015, 0);
            assert!(apu.irq_line());
            assert_eq!(apu.read_status() & 0x40, 0x40);
        }

        #[test]
        fn inhibit_write_clears_irq_without_waiting_for_reset() {
            let mut apu = Apu::new();
            advance(&mut apu, 29_831);
            assert!(apu.irq_line());
            apu.write_register(0x4017, 0x40);
            assert!(!apu.irq_line(), "bit6 应立即撤销 IRQ 请求");
            advance(&mut apu, 60_000);
            assert!(!apu.irq_line());
            assert_eq!(apu.read_status() & 0x40, 0);
        }

        #[test]
        fn writing_bit6_clear_does_not_clear_an_existing_irq() {
            for value in [0x00, 0x80] {
                let mut apu = Apu::new();
                advance(&mut apu, 29_831);
                apu.write_register(0x4017, value);
                assert!(apu.irq_line());
                advance(&mut apu, 4);
                assert!(
                    apu.irq_line(),
                    "value={value:#04X}，重置/切换模式不确认旧 IRQ"
                );
            }
        }

        #[test]
        fn five_step_mode_does_not_generate_new_frame_irqs() {
            let mut apu = Apu::new();
            apu.write_register(0x4017, 0x80);
            for cycle in 1..=100_000 {
                apu.clock_cpu_cycle();
                assert!(!apu.irq_line(), "cycle={cycle}");
            }
        }

        fn make_nes() -> Nes {
            let mut rom = vec![0u8; 16 + 0x4000];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;
            let prg = &mut rom[16..];
            // $8000: CLI; $8001: JMP $8001。
            prg[..4].copy_from_slice(&[0x58, 0x4C, 0x01, 0x80]);
            // $9000: LDA $4015; INC $10; RTI。
            prg[0x1000..0x1006].copy_from_slice(&[0xAD, 0x15, 0x40, 0xE6, 0x10, 0x40]);
            prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
            prg[0x3FFE..0x4000].copy_from_slice(&0x9000u16.to_le_bytes());
            let mut bus = Bus::new();
            bus.insert_cartridge(Cartridge::try_from(rom.as_slice()).unwrap());
            let mut cpu = Cpu::new();
            cpu.reset(&mut bus).unwrap();
            Nes::new(cpu, bus)
        }

        #[test]
        fn nes_services_and_acknowledges_apu_irq_without_reentering() {
            let mut nes = make_nes();
            assert_eq!(nes.step().unwrap(), 2); // CLI。
            assert_eq!(nes.step().unwrap(), 3); // CLI 后允许继续的一条指令。
            for _ in 0..29_826 {
                nes.clock_one_cpu_cycle().unwrap();
            }
            assert_eq!(nes.cpu_cycles(), 29_831);
            assert!(nes.bus.irq_line());
            assert_eq!(nes.step().unwrap(), 7); // IRQ 入口，不是执行处理程序。
            assert!(nes.bus.irq_line(), "CPU 进入 IRQ 不能替 APU 确认请求");
            assert_eq!(nes.step().unwrap(), 4); // LDA $4015。
            assert!(!nes.bus.irq_line());
            assert_eq!(nes.step().unwrap(), 5); // INC $10。
            assert_eq!(nes.bus.cpu_read(0x10).unwrap(), 1);
            assert_eq!(nes.step().unwrap(), 6); // RTI。
            assert_eq!(nes.step().unwrap(), 3); // 恢复 JMP，而不是再次进入 IRQ。
        }

        #[test]
        fn cpu_i_flag_blocks_entry_but_does_not_erase_apu_request() {
            let mut nes = make_nes(); // reset 后 I=1。
            for _ in 0..29_831 {
                nes.clock_one_cpu_cycle().unwrap();
            }
            assert!(nes.bus.irq_line());
            assert_eq!(nes.step().unwrap(), 2); // 仍执行 CLI，而非 IRQ 入口。
            assert_eq!(nes.step().unwrap(), 3); // CLI 的旧 I 采样。
            assert!(nes.bus.irq_line());
            assert_eq!(nes.step().unwrap(), 7); // 随后才进入 IRQ。
        }
    }

    mod clocked_cpu_bus_tests {

        use super::*;

        #[test]
        fn adapter_clocks_each_read_and_write_once() {
            let mut bus = Bus::new();
            let mut cycles = 0u64;

            // 普通 Bus 访问不自动计时。
            bus.cpu_write(0x0010, 0x42).unwrap();
            assert_eq!(bus.ppu().dot(), 0);

            {
                let mut dmc_dma = DmcDma::new();
                let mut clocked = ClockedCpuBus::new(&mut bus, &mut cycles, &mut dmc_dma);

                assert_eq!(clocked.cpu_read(0x0010).unwrap(), 0x42);
                clocked.cpu_write(0x0011, 0x99).unwrap();
            }

            assert_eq!(cycles, 2);
            assert_eq!(bus.ppu().dot(), 6);
            assert_eq!(bus.cpu_read(0x0011).unwrap(), 0x99);
            assert_eq!(bus.ppu().dot(), 6);
        }

        #[test]
        fn instructions_do_not_clock_twice() {
            // LDA #$42；STA $0010；NOP
            let mut nes = nes_with_program(&[0xA9, 0x42, 0x8D, 0x10, 0x00, 0xEA]);

            for (cycles, total) in [(2, 2u64), (4, 6), (2, 8)] {
                assert_eq!(nes.step().unwrap(), cycles);
                assert_eq!(nes.cpu_cycles(), total);
                assert_eq!(nes.bus().ppu().dot(), (total * 3) as u16);
            }

            assert_eq!(nes.bus.cpu_read(0x0010).unwrap(), 0x42);
        }

        fn advance_near_vblank(nes: &mut Nes) {
            // 27392 * 3 = 82176 = 240 * 341 + 336。
            for _ in 0..27_392 {
                nes.clock_one_cpu_cycle().unwrap();
            }

            assert_eq!(nes.bus().ppu().scanline(), 240);
            assert_eq!(nes.bus().ppu().dot(), 336);
        }

        #[test]
        fn status_read_sees_ppu_progress_during_instruction() {
            // LDA $2002；STA $00
            let mut nes = nes_with_program(&[0xAD, 0x02, 0x20, 0x85, 0x00]);

            advance_near_vblank(&mut nes);
            let before = nes.cpu_cycles();

            assert_eq!(nes.step().unwrap(), 4);
            assert_eq!(nes.cpu_cycles(), before + 4);
            assert_eq!(nes.step().unwrap(), 3);

            // 前三个读取周期推进了 PPU；
            // 第四个周期读取状态时，VBlank 已经置位。
            let saved_status = nes.bus.cpu_read(0x0000).unwrap();
            assert_eq!(saved_status & 0x80, 0x80);

            // LDA $2002 已经清除了 VBlank。
            assert_eq!(nes.bus.cpu_read(0x2002).unwrap() & 0x80, 0);
        }

        #[test]
        fn nmi_edge_survives_status_read_in_same_instruction() {
            // LDA $2002；NOP
            let mut nes = nes_with_program(&[0xAD, 0x02, 0x20, 0xEA]);

            // 现有测试 ROM 的 NMI 向量默认为 $0000。
            // 在那里放置 RTI，作为测试中断处理程序。
            nes.bus.cpu_write(0x0000, 0x40).unwrap();

            // 开启 VBlank NMI，但不启用画面渲染。
            nes.bus.cpu_write(0x2000, 0x80).unwrap();
            advance_near_vblank(&mut nes);

            assert_eq!(nes.step().unwrap(), 4);

            // 指令中间产生过上升沿，随后被 $2002 读取拉低。
            assert!(!nes.bus().ppu().nmi_line());

            // 即使最终电平为低，仍应处理已锁存的 NMI。
            assert_eq!(nes.step().unwrap(), 7);
            assert_eq!(nes.bus.cpu_read(0x01FD).unwrap(), 0x80);
            assert_eq!(nes.bus.cpu_read(0x01FC).unwrap(), 0x03);

            assert_eq!(nes.step().unwrap(), 6); // RTI
            assert_eq!(nes.step().unwrap(), 2); // 原程序的 NOP
        }

        #[test]
        fn apu_advances_with_cpu_accesses() {
            let mut nes = nes_with_program(&[0xEA]);

            for _ in 0..29_826 {
                nes.clock_one_cpu_cycle().unwrap();
            }
            assert!(!nes.bus.irq_line());

            // 当前帧计数器在第 29828 个周期首次置位 IRQ。
            assert_eq!(nes.step().unwrap(), 2);
            assert_eq!(nes.cpu_cycles(), 29_828);
            assert!(nes.bus.irq_line());
        }

        #[test]
        fn failed_access_preserves_already_elapsed_cycles() {
            // $6000 在当前 Bus 中尚未实现。
            let mut nes = nes_with_program(&[0xAD, 0x00, 0x60]);

            assert!(nes.step().is_err());

            // 操作码和两个操作数字节已经读取成功；
            // 失败的第四次访问不额外推进周期。
            assert_eq!(nes.cpu_cycles(), 3);
            assert_eq!(nes.bus().ppu().dot(), 9);
        }
    }

    mod dmc_bus_execution_tests {
        use super::*;
        use crate::dmc_dma::DmaPhase;

        fn fixture(program: &[u8]) -> Nes {
            assert!(program.len() < 0x80);
            let mut rom = vec![0u8; 16 + 0x4000];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;
            rom[16..16 + program.len()].copy_from_slice(program);

            // Mapper 0 的 16 KiB PRG 镜像：CPU $C080 对应 PRG 偏移 $0080。
            rom[16 + 0x80] = 0xA5;
            rom[16 + 0x3FFC..16 + 0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());

            let cartridge = Cartridge::try_from(rom.as_slice()).unwrap();
            let mut bus = Bus::new();
            bus.insert_cartridge(cartridge);
            let mut cpu = Cpu::new();
            cpu.reset(&mut bus).unwrap();
            let mut nes = Nes::new(cpu, bus);

            // IRQ 开启、不循环、速率 0；输出起点 64。
            // 样本从 $C080 开始，长度为 1 字节。
            // 本轮普通 Bus 写入只配置 DMC，不自动调度 DMA。
            for (addr, value) in [
                (0x4010, 0x80),
                (0x4011, 64),
                (0x4012, 2),
                (0x4013, 0),
                (0x4015, 0x10),
            ] {
                nes.bus.cpu_write(addr, value).unwrap();
            }
            nes.bus.take_read_trace();
            nes.bus.take_write_trace();
            nes
        }

        fn align_next_cycle_to_get(nes: &mut Nes) {
            if nes.cpu_cycles() & 1 == 0 {
                nes.clock_one_cpu_cycle().unwrap();
            }
            assert_eq!(nes.cpu_cycles() & 1, 1);
        }

        #[test]
        fn load_survives_instruction_boundary_and_resumes_same_read() {
            let mut nes = fixture(&[0xEA, 0xEA, 0xEA]);
            align_next_cycle_to_get(&mut nes);
            let before = nes.cpu_cycles();
            let dot_before = nes.bus().ppu().dot();

            // 模拟在前一个 Put 周期发出的 load 请求。
            nes.dmc_dma.request_load(0xC080, DmaPhase::Put).unwrap();

            // 两个等待周期允许第一条 NOP 完成，请求跨指令保存。
            assert_eq!(nes.step().unwrap(), 2);
            assert_eq!(nes.cpu_cycles() - before, 2);
            assert!(nes.dmc_dma.is_busy());
            assert_eq!(nes.bus.take_read_trace(), vec![0x8000, 0x8001]);

            // 第二条 NOP：三周期 DMA 停顿 + 两周期指令。
            assert_eq!(nes.step().unwrap(), 2);
            assert_eq!(nes.cpu_cycles() - before, 7);
            assert!(!nes.dmc_dma.is_busy());
            assert_eq!(nes.bus().ppu().dot() - dot_before, 21);
            assert_eq!(
                nes.bus.take_read_trace(),
                vec![
                    0x8001, // Halt：重复待完成的 CPU 读取。
                    0x8001, // Dummy。
                    0xC080, // DMA 读取样本，不能返回给 CPU。
                    0x8001, // Pass：CPU 真正取得第二条 NOP 的操作码。
                    0x8002, // NOP 的第二个周期。
                ]
            );

            // DMC active 清零，样本读取结束 IRQ 置位。
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x90, 0x80);
            assert!(nes.bus.irq_line());
            assert_eq!(nes.bus.dmc_output(), 64); // 已取到样本不等于已播放。

            // 请求不能在下一条指令中重复执行。
            nes.bus.take_read_trace();
            let before = nes.cpu_cycles();
            assert_eq!(nes.step().unwrap(), 2);
            assert_eq!(nes.cpu_cycles() - before, 2);
            assert_eq!(nes.bus.take_read_trace(), vec![0x8002, 0x8003]);
        }

        #[test]
        fn load_due_on_write_allows_write_then_halts_next_read() {
            // A 初值为 0；STA $0010；NOP。
            let mut nes = fixture(&[0x8D, 0x10, 0x00, 0xEA]);
            assert_eq!(nes.cpu_cycles() & 1, 0); // 下一个周期 Put。
            nes.dmc_dma.request_load(0xC080, DmaPhase::Get).unwrap();

            // 第四个周期到期，但该周期是写，必须正常完成。
            assert_eq!(nes.step().unwrap(), 4);
            assert_eq!(nes.cpu_cycles(), 4);
            assert!(nes.dmc_dma.is_busy());
            assert_eq!(nes.bus.take_write_trace(), vec![(0x0010, 0)]);
            assert_eq!(nes.bus.take_read_trace(), vec![0x8000, 0x8001, 0x8002]);

            // 下一个 Put 周期暂停：Halt、Dummy、Align、Read 共四周期。
            assert_eq!(nes.step().unwrap(), 2);
            assert_eq!(nes.cpu_cycles(), 10);
            assert_eq!(nes.bus().ppu().dot(), 30);
            assert_eq!(
                nes.bus.take_read_trace(),
                vec![0x8003, 0x8003, 0x8003, 0xC080, 0x8003, 0x8004]
            );
            assert!(nes.bus.take_write_trace().is_empty());
            assert!(!nes.dmc_dma.is_busy());
        }

        #[test]
        fn reload_waits_for_put_and_inserts_four_stalled_cycles() {
            let mut nes = fixture(&[0xEA, 0xEA]);
            align_next_cycle_to_get(&mut nes);
            let before = nes.cpu_cycles();
            nes.dmc_dma.request_reload(0xC080).unwrap();

            // Get 周期先执行操作码读取，Put 周期暂停 NOP 的第二次读取。
            assert_eq!(nes.step().unwrap(), 2);
            assert_eq!(nes.cpu_cycles() - before, 6);
            assert_eq!(
                nes.bus.take_read_trace(),
                vec![0x8000, 0x8001, 0x8001, 0x8001, 0xC080, 0x8001]
            );
            assert!(!nes.dmc_dma.is_busy());

            let before = nes.cpu_cycles();
            assert_eq!(nes.step().unwrap(), 2);
            assert_eq!(nes.cpu_cycles() - before, 2);
        }

        #[test]
        fn dma_delivers_sample_and_apu_keeps_running_during_stalls() {
            let mut nes = fixture(&[0xEA, 0xEA]);
            align_next_cycle_to_get(&mut nes);
            nes.dmc_dma.request_load(0xC080, DmaPhase::Put).unwrap();
            nes.step().unwrap();
            nes.step().unwrap();
            assert_eq!(nes.bus.dmc_output(), 64);

            // 现有 DMC timer 初值 0，速率 0 每 428 CPU 周期移出一位。
            // 第 1、429、857……周期处理初始静音组；
            // 第 2997 周期将 $A5 装入移位寄存器；
            // 第 3425 周期消费它的 bit 0（1），输出从 64 变为 66。
            while nes.cpu_cycles() < 3424 {
                nes.clock_one_cpu_cycle().unwrap();
            }
            assert_eq!(nes.bus.dmc_output(), 64);
            nes.clock_one_cpu_cycle().unwrap();
            assert_eq!(nes.bus.dmc_output(), 66);
        }
    }

    mod dmc_auto_request_tests {
        use super::*;

        fn fixture(program: &[u8], control: u8, length: u8) -> Nes {
            assert!(program.len() < 0x80);
            let mut rom = vec![0u8; 16 + 0x4000];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;
            let prg = &mut rom[16..];
            prg[..program.len()].copy_from_slice(program);
            // 16 KiB NROM 镜像：PRG 偏移 $80 对应 CPU 地址 $C080。
            prg[0x80] = 0xA5;
            prg[0x81] = 0x3C;
            prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());

            let cartridge = Cartridge::try_from(rom.as_slice()).unwrap();
            let mut bus = Bus::new();
            bus.insert_cartridge(cartridge);
            bus.cpu_write(0x0000, 0x10).unwrap();
            bus.cpu_write(0x4010, control).unwrap();
            bus.cpu_write(0x4011, 64).unwrap();
            bus.cpu_write(0x4012, 2).unwrap(); // $C080。
            bus.cpu_write(0x4013, length).unwrap();
            bus.cpu_write(0x4017, 0x40).unwrap(); // 禁止 frame IRQ。
            // 不在这里写 $4015；由下面的 CPU 程序启用 DMC。
            let mut cpu = Cpu::new();
            cpu.reset(&mut bus).unwrap();
            bus.take_read_trace();
            Nes::new(cpu, bus)
        }

        fn sample_reads(nes: &mut Nes) -> Vec<u16> {
            nes.bus
                .take_read_trace()
                .into_iter()
                .filter(|addr| (0xC080u16..=0xC090).contains(addr))
                .collect()
        }

        fn run_to(nes: &mut Nes, target: u64) {
            // 指令可能跨过 target；这些测试的时间边界留有足够余量。
            for _ in 0..20_000 {
                if nes.cpu_cycles() >= target {
                    return;
                }
                nes.step().unwrap();
            }
            panic!("did not reach {target} CPU cycles");
        }

        fn enable_program() -> [u8; 8] {
            // LDA #$10; STA $4015; JMP $8005。
            [0xA9, 0x10, 0x8D, 0x15, 0x40, 0x4C, 0x05, 0x80]
        }

        #[test]
        fn cpu_enable_schedules_load_with_both_write_phases() {
            for (load_opcode, operand, load_cycles, after_write) in [
                (0xA9u8, 0x10u8, 2u8, 6u64), // 写周期开始计数 5：Get。
                (0xA5u8, 0x00u8, 3u8, 7u64), // 写周期开始计数 6：Put。
            ] {
                // LDA; STA $4015; NOP; NOP; JMP $8007。
                let program = [
                    load_opcode,
                    operand,
                    0x8D,
                    0x15,
                    0x40,
                    0xEA,
                    0xEA,
                    0x4C,
                    0x07,
                    0x80,
                ];
                let mut nes = fixture(&program, 0x80, 0);
                assert_eq!(nes.step().unwrap(), load_cycles);
                assert_eq!(nes.step().unwrap(), 4);
                assert_eq!(nes.cpu_cycles(), after_write);
                assert!(nes.dmc_dma.is_busy());
                assert!(sample_reads(&mut nes).is_empty());

                // 两种相位下，写入后的第一个 NOP 都尚未被暂停。
                assert_eq!(nes.step().unwrap(), 2);
                assert_eq!(nes.cpu_cycles(), after_write + 2);
                assert!(sample_reads(&mut nes).is_empty());

                // 第二个 NOP 增加 3 个 DMA 停顿周期，但返回的指令周期仍为 2。
                assert_eq!(nes.step().unwrap(), 2);
                assert_eq!(nes.cpu_cycles(), after_write + 7);
                assert_eq!(sample_reads(&mut nes), vec![0xC080]);
                assert!(!nes.dmc_dma.is_busy());
                assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x90, 0x80);
            }
        }

        #[test]
        fn disabled_dmc_does_not_request_samples() {
            // LDA #$00; STA $4015; JMP $8005。
            let program = [0xA9, 0x00, 0x8D, 0x15, 0x40, 0x4C, 0x05, 0x80];
            let mut nes = fixture(&program, 0x00, 1);
            run_to(&mut nes, 4_000);
            assert!(sample_reads(&mut nes).is_empty());
            assert!(!nes.dmc_dma.is_busy());
        }

        #[test]
        fn draining_buffer_automatically_fetches_next_byte() {
            let mut nes = fixture(&enable_program(), 0x00, 1); // 17 字节，rate 0。
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
            assert!(!nes.dmc_dma.is_busy());

            run_to(&mut nes, 2_900);
            assert!(sample_reads(&mut nes).is_empty()); // 缓冲还没被取走。

            run_to(&mut nes, 3_100);
            assert_eq!(sample_reads(&mut nes), vec![0xC081]);
            assert!(!nes.dmc_dma.is_busy());

            run_to(&mut nes, 6_000);
            assert!(sample_reads(&mut nes).is_empty()); // 第二个字节仍在缓冲里。
            run_to(&mut nes, 6_600);
            assert_eq!(sample_reads(&mut nes), vec![0xC082]);
        }

        #[test]
        fn final_byte_does_not_schedule_another_reload() {
            let mut nes = fixture(&enable_program(), 0x80, 0); // 单字节、IRQ 开启。
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x90, 0x80);

            run_to(&mut nes, 8_000);
            assert!(sample_reads(&mut nes).is_empty());
            assert!(!nes.dmc_dma.is_busy());
        }

        #[test]
        fn looped_sample_reloads_only_when_buffer_is_empty() {
            let mut nes = fixture(&enable_program(), 0x40, 0); // 单字节循环，无 IRQ。
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);

            run_to(&mut nes, 2_900);
            assert!(sample_reads(&mut nes).is_empty());
            run_to(&mut nes, 3_100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x90, 0x10);
        }

        #[test]
        fn repeated_enable_does_not_restart_active_sample() {
            // LDA #$10; STA $4015; STA $4015; JMP $8008。
            let program = [
                0xA9, 0x10, 0x8D, 0x15, 0x40, 0x8D, 0x15, 0x40, 0x4C, 0x08, 0x80,
            ];
            let mut nes = fixture(&program, 0x00, 1);
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
            run_to(&mut nes, 3_100);
            assert_eq!(sample_reads(&mut nes), vec![0xC081]);
        }

        #[test]
        fn restarting_with_full_buffer_waits_for_reload() {
            // 第一字节已被读入缓冲，但尚未送入移位寄存器时再次启用。
            // LDA #$10; STA $4015; NOP; NOP; STA $4015; JMP $800A。
            let program = [
                0xA9, 0x10, 0x8D, 0x15, 0x40, 0xEA, 0xEA, 0x8D, 0x15, 0x40, 0x4C, 0x0A, 0x80,
            ];
            let mut nes = fixture(&program, 0x00, 0);
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
            assert!(!nes.dmc_dma.is_busy());
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x10, 0x10);

            run_to(&mut nes, 2_900);
            assert!(sample_reads(&mut nes).is_empty());
            run_to(&mut nes, 3_100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
        }
    }

    mod dmc_stop_restart_tests {
        use super::*;

        fn fixture(length: u8) -> Nes {
            let mut rom = vec![0u8; 16 + 0x4000];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;
            let prg = &mut rom[16..];
            prg[0x80] = 0xA5; // CPU $C080。
            prg[0xC0] = 0x3C; // CPU $C0C0。
            prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
            let mut bus = Bus::new();
            bus.insert_cartridge(Cartridge::try_from(rom.as_slice()).unwrap());
            bus.cpu_write(0x4010, 0x80).unwrap(); // IRQ 开启、无循环、rate 0。
            bus.cpu_write(0x4011, 64).unwrap();
            bus.cpu_write(0x4012, 2).unwrap();
            bus.cpu_write(0x4013, length).unwrap();
            bus.cpu_write(0x4017, 0x40).unwrap(); // 禁止 frame IRQ。
            let mut cpu = Cpu::new();
            cpu.reset(&mut bus).unwrap();
            bus.take_read_trace();
            Nes::new(cpu, bus)
        }

        fn write(nes: &mut Nes, addr: u16, value: u8) {
            let mut adapter =
                ClockedCpuBus::new(&mut nes.bus, &mut nes.cpu_cycles, &mut nes.dmc_dma);
            adapter.cpu_write(addr, value).unwrap();
            // 本组不执行 CPU 指令，仅验证 DMA/APU，不回放 NMI。
        }

        fn read_ram(nes: &mut Nes) {
            let mut adapter =
                ClockedCpuBus::new(&mut nes.bus, &mut nes.cpu_cycles, &mut nes.dmc_dma);
            adapter.cpu_read(0x0000).unwrap();
        }

        fn run_to(nes: &mut Nes, target: u64) {
            for _ in 0..20_000 {
                if nes.cpu_cycles() >= target {
                    return;
                }
                read_ram(nes);
            }
            panic!("did not reach {target} CPU cycles");
        }

        fn sample_reads(nes: &mut Nes) -> Vec<u16> {
            nes.bus
                .take_read_trace()
                .into_iter()
                .filter(|addr| (0xC080u16..=0xC0FF).contains(addr))
                .collect()
        }

        #[test]
        fn disabling_pending_load_cancels_it_on_both_phases() {
            for prefix_cycles in [0, 1] {
                let mut nes = fixture(1);
                if prefix_cycles == 1 {
                    write(&mut nes, 0x0000, 0);
                }
                write(&mut nes, 0x4015, 0x10);
                assert!(nes.dmc_dma.is_busy());
                let before = nes.cpu_cycles();
                write(&mut nes, 0x4015, 0x00);
                assert_eq!(nes.cpu_cycles(), before + 1);
                assert!(!nes.dmc_dma.is_busy());
                assert!(!nes.bus.dmc_active());
                assert_eq!(nes.bus.dmc_next_sample_address(), None);
                read_ram(&mut nes);
                assert_eq!(nes.cpu_cycles(), before + 2);
                assert!(sample_reads(&mut nes).is_empty());
            }
        }

        #[test]
        fn stop_write_cancels_reload_even_when_halt_attempt_is_due() {
            let mut nes = fixture(1);
            write(&mut nes, 0x4015, 0x10);
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);

            // 当前初始 timer=0、bits_remaining=8：2997 时缓冲进入移位寄存器。
            run_to(&mut nes, 2997);
            assert_eq!(nes.cpu_cycles(), 2997);
            assert!(nes.dmc_dma.is_busy());
            assert_eq!(nes.bus.dmc_next_sample_address(), Some(0xC081));

            // 下一周期 Get：reload 尚不尝试 Halt，正常写 RAM。
            write(&mut nes, 0x0000, 0);
            assert_eq!(nes.cpu_cycles(), 2998);
            // 下一周期 Put：尝试 Halt，但 CPU 正在写 $4015，先放行再取消。
            write(&mut nes, 0x4015, 0);
            assert_eq!(nes.cpu_cycles(), 2999);
            assert!(!nes.dmc_dma.is_busy());
            read_ram(&mut nes);
            assert_eq!(nes.cpu_cycles(), 3000);
            assert!(sample_reads(&mut nes).is_empty());
        }

        #[test]
        fn restart_after_cancellation_fetches_new_sample_not_stale_address() {
            let mut nes = fixture(1);
            write(&mut nes, 0x4015, 0x10);
            write(&mut nes, 0x4015, 0x00);
            write(&mut nes, 0x4012, 3); // 新起点 $C0C0。
            write(&mut nes, 0x4013, 0); // 新长度 1。
            write(&mut nes, 0x4015, 0x10);
            assert!(nes.dmc_dma.is_busy());
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC0C0]);
            assert!(!nes.dmc_dma.is_busy());
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x90, 0x80);
        }

        #[test]
        fn disabling_preserves_buffer_and_output_without_fetching_more() {
            let mut nes = fixture(1);
            write(&mut nes, 0x4015, 0x10);
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
            assert_eq!(nes.bus.dmc_output(), 64);
            write(&mut nes, 0x4015, 0);
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x90, 0);
            assert_eq!(nes.bus.dmc_output(), 64);

            run_to(&mut nes, 3424);
            assert_eq!(nes.bus.dmc_output(), 64);
            read_ram(&mut nes); // 3425：旧样本 $A5 的最低位为 1，64 -> 66。
            assert_eq!(nes.cpu_cycles(), 3425);
            assert_eq!(nes.bus.dmc_output(), 66);
            run_to(&mut nes, 7000); // $A5 四个 1、四个 0，最终回到 64 并保持。
            assert_eq!(nes.bus.dmc_output(), 64);
            assert!(sample_reads(&mut nes).is_empty());
            assert!(!nes.dmc_dma.is_busy());
        }

        #[test]
        fn restart_preserves_old_buffer_then_refills_from_new_start() {
            let mut nes = fixture(1);
            write(&mut nes, 0x4015, 0x10);
            run_to(&mut nes, 100);
            assert_eq!(sample_reads(&mut nes), vec![0xC080]);
            write(&mut nes, 0x4015, 0);
            write(&mut nes, 0x4012, 3);
            write(&mut nes, 0x4013, 0);
            write(&mut nes, 0x4015, 0x10);
            // 仍保留 $A5：不能立刻用新起点的 $3C 覆盖缓冲。
            assert!(!nes.dmc_dma.is_busy());
            assert!(nes.bus.dmc_active());
            assert_eq!(nes.bus.dmc_next_sample_address(), None);
            run_to(&mut nes, 2900);
            assert!(sample_reads(&mut nes).is_empty());
            run_to(&mut nes, 3100);
            assert_eq!(sample_reads(&mut nes), vec![0xC0C0]);
            run_to(&mut nes, 3425);
            assert_eq!(nes.bus.dmc_output(), 66); // 播放的是旧 $A5，不是新 $3C。
        }

        #[test]
        fn disabling_clears_dmc_irq_but_keeps_output_level() {
            let mut nes = fixture(0);
            write(&mut nes, 0x4015, 0x10);
            run_to(&mut nes, 100);
            assert!(nes.bus.irq_line());
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x80, 0x80);
            write(&mut nes, 0x4015, 0);
            assert!(!nes.bus.irq_line());
            assert_eq!(nes.bus.cpu_read(0x4015).unwrap() & 0x90, 0);
            assert_eq!(nes.bus.dmc_output(), 64);
        }
    }

    // Append this whole module at FILE SCOPE in src/nes.rs.
    // It replaces the old oam_dma_tests module that directly invoked
    // run_pending_oam_dma(); keep the standalone oam_dma.rs tests unchanged.
    // ClockedCpuBus::new retains its existing three arguments.
    // No new production getter or tracing API is required.
    // These tests have NOT been run by the assistant.
    #[cfg(test)]
    mod oam_dmc_shared_bus_tests {
        use super::{ClockedCpuBus, Nes};
        use crate::{bus::Bus, cartridge::Cartridge, cpu::Cpu, cpu_bus::CpuBus};

        fn source_byte(page: u8, offset: u8) -> u8 {
            offset.rotate_left(3) ^ page ^ 0xA5
        }

        fn fixture(program: &[u8]) -> Nes {
            assert!(program.len() < 0x80);
            let mut rom = vec![0u8; 16 + 0x4000];
            rom[..4].copy_from_slice(b"NES\x1A");
            rom[4] = 1;
            let prg = &mut rom[16..];
            prg[..program.len()].copy_from_slice(program);
            prg[0x80] = 0xA5; // CPU $C080, using the 16 KiB PRG mirror.
            prg[0x81] = 0x3C; // CPU $C081.
            prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());

            let mut bus = Bus::new();
            bus.insert_cartridge(Cartridge::try_from(rom.as_slice()).unwrap());
            for page in [0x02u8, 0x03] {
                for offset in 0..256u16 {
                    bus.cpu_write(
                        (u16::from(page) << 8) | offset,
                        source_byte(page, offset as u8),
                    )
                    .unwrap();
                }
            }
            bus.cpu_write(0x0000, 0x02).unwrap();
            bus.cpu_write(0x2001, 0).unwrap(); // Rendering off for these tests.
            bus.cpu_write(0x2003, 0xFC).unwrap(); // Exercise OAM destination wrap.
            bus.cpu_write(0x4010, 0).unwrap(); // Rate 0: 428 CPU cycles per bit.
            bus.cpu_write(0x4011, 64).unwrap();
            bus.cpu_write(0x4012, 2).unwrap(); // Sample begins at $C080.
            bus.cpu_write(0x4013, 1).unwrap(); // 17 sample bytes.
            bus.cpu_write(0x4017, 0x40).unwrap(); // Frame IRQ inhibited.

            let mut cpu = Cpu::new();
            cpu.reset(&mut bus).unwrap();
            bus.take_read_trace();
            bus.take_write_trace();
            Nes::new(cpu, bus)
        }

        // Exercise the real clocked adapter, rather than an OAM-specific helper.
        // No interrupt is pending in these fixtures, so the captured NMI history
        // can be dropped here. Production Nes::step must retain its NMI replay.
        fn clocked_read(nes: &mut Nes, addr: u16) -> u8 {
            let mut bus = ClockedCpuBus::new(&mut nes.bus, &mut nes.cpu_cycles, &mut nes.dmc_dma);
            CpuBus::cpu_read(&mut bus, addr).unwrap()
        }

        fn clocked_write(nes: &mut Nes, addr: u16, value: u8) {
            let mut bus = ClockedCpuBus::new(&mut nes.bus, &mut nes.cpu_cycles, &mut nes.dmc_dma);
            CpuBus::cpu_write(&mut bus, addr, value).unwrap();
        }

        fn clear_traces(nes: &mut Nes) {
            nes.bus.take_read_trace();
            nes.bus.take_write_trace();
        }

        fn assert_oam_writes_and_image(nes: &mut Nes, data_page: u8) {
            let writes: Vec<u8> = nes
                .bus
                .take_write_trace()
                .into_iter()
                .filter_map(|(addr, value)| (addr == 0x2004).then_some(value))
                .collect();
            let expected: Vec<u8> = (0..256u16)
                .map(|offset| source_byte(data_page, offset as u8))
                .collect();
            assert_eq!(writes, expected, "exactly 256 ordered writes to $2004");

            // OAMADDR must wrap back to $FC after 256 writes. OAMDATA reads
            // do not increment it; inspect it before explicitly selecting addresses.
            assert_eq!(nes.bus.cpu_read(0x2004).unwrap(), source_byte(data_page, 0));
            for offset in 0..256u16 {
                let destination = 0xFCu8.wrapping_add(offset as u8);
                nes.bus.cpu_write(0x2003, destination).unwrap();
                let value = source_byte(data_page, offset as u8);
                let stored = if destination & 3 == 2 {
                    value & 0xE3
                } else {
                    value
                };
                assert_eq!(
                    nes.bus.cpu_read(0x2004).unwrap(),
                    stored,
                    "OAM destination ${destination:02X}, source offset {offset}"
                );
            }
        }

        #[test]
        fn sta_4014_only_queues_dma_and_next_cpu_read_performs_it() {
            for (load, operand, load_cycles, after_sta, stall) in [
                (0xA9, 0x02, 2, 6u64, 513u64), // Next cycle is Put.
                (0xA5, 0x00, 3, 7u64, 514u64), // Next cycle is Get.
            ] {
                let mut nes = fixture(&[load, operand, 0x8D, 0x14, 0x40, 0xEA]);
                assert_eq!(nes.step().unwrap(), load_cycles);
                assert_eq!(nes.step().unwrap(), 4);
                assert_eq!(
                    nes.cpu_cycles(),
                    after_sta,
                    "STA must not eagerly drain OAM DMA at instruction end"
                );
                assert!(!nes.bus.take_write_trace().iter().any(|&(a, _)| a == 0x2004));
                nes.bus.take_read_trace();

                assert_eq!(nes.step().unwrap(), 2); // NOP still reports 2 nominal cycles.
                assert_eq!(nes.cpu_cycles(), after_sta + stall + 2);
                let reads = nes.bus.take_read_trace();
                let source: Vec<u16> = reads
                    .iter()
                    .copied()
                    .filter(|a| (0x0200..=0x02FF).contains(a))
                    .collect();
                assert_eq!(source, (0x0200u16..=0x02FF).collect::<Vec<_>>());
                assert_eq!(
                    &reads[reads.len() - 2..],
                    &[0x8005, 0x8006],
                    "CPU resumes the held opcode read, then NOP's dummy read"
                );
                assert_oam_writes_and_image(&mut nes, 0x02);
            }
        }

        #[test]
        fn consecutive_cpu_writes_continue_and_last_oam_page_wins() {
            let mut nes = fixture(&[0xEA]);
            // Prepare a real DMC reader, but use a low-level request to place
            // its halt attempt exactly on the first CPU write below.
            nes.bus.cpu_write(0x4015, 0x10).unwrap();
            nes.dmc_dma.request_reload(0xC080).unwrap();
            clear_traces(&mut nes);

            clocked_write(&mut nes, 0x4014, 0x02);
            clocked_write(&mut nes, 0x4014, 0x03);
            assert_eq!(nes.cpu_cycles(), 2);
            assert!(nes.dmc_dma.is_busy());
            assert!(nes.bus.take_read_trace().is_empty());
            assert_eq!(nes.bus.take_write_trace(), vec![(0x4014, 2), (0x4014, 3)]);

            let before = nes.cpu_cycles();
            assert_eq!(clocked_read(&mut nes, 0x8000), 0xEA);
            assert_eq!(nes.cpu_cycles() - before, 516); // 513 + overlap 2 + CPU read 1.
            let reads = nes.bus.take_read_trace();
            assert_eq!(reads.iter().filter(|&&a| a == 0xC080).count(), 1);
            assert!(!reads.iter().any(|a| (0x0200..=0x02FF).contains(a)));
            assert_eq!(
                reads
                    .iter()
                    .copied()
                    .filter(|a| (0x0300..=0x03FF).contains(a))
                    .collect::<Vec<_>>(),
                (0x0300u16..=0x03FF).collect::<Vec<_>>()
            );
            assert_oam_writes_and_image(&mut nes, 0x03);
        }

        #[test]
        fn oam_source_uses_cpu_ram_mirrors() {
            let mut nes = fixture(&[0xEA]);
            clocked_write(&mut nes, 0x4014, 0x0A); // $0A00 mirrors $0200.
            clear_traces(&mut nes);
            let before = nes.cpu_cycles();
            assert_eq!(clocked_read(&mut nes, 0x8000), 0xEA);
            assert_eq!(nes.cpu_cycles() - before, 515); // 514 stall + 1 resumed read.
            let reads = nes.bus.take_read_trace();
            assert_eq!(
                reads
                    .into_iter()
                    .filter(|a| (0x0A00..=0x0AFF).contains(a))
                    .collect::<Vec<_>>(),
                (0x0A00u16..=0x0AFF).collect::<Vec<_>>()
            );
            assert_oam_writes_and_image(&mut nes, 0x02);
        }

        #[test]
        fn shared_halt_dummy_and_alignment_do_not_duplicate_bus_reads() {
            let mut nes = fixture(&[0xEA]);
            nes.bus.cpu_write(0x4015, 0x10).unwrap();
            nes.dmc_dma.request_reload(0xC080).unwrap();
            nes.bus.cpu_write(0x4014, 0x02).unwrap();
            clear_traces(&mut nes);

            assert_eq!(clocked_read(&mut nes, 0x8000), 0xEA);
            assert_eq!(nes.cpu_cycles(), 516);
            let reads = nes.bus.take_read_trace();
            // Shared Halt; OAM read during DMC Dummy; DMC sample read;
            // OAM alignment; remaining OAM reads; resumed CPU read.
            let mut expected = vec![0x8000, 0x0200, 0xC080, 0x8000];
            expected.extend(0x0201u16..=0x02FF);
            expected.push(0x8000);
            assert_eq!(reads, expected);
            assert!(!nes.dmc_dma.is_busy());
            assert_oam_writes_and_image(&mut nes, 0x02);
        }

        // Real APU clocks empty the existing sample buffer and automatically
        // request $C081 while OAM is in progress. No request_reload() here.
        // With the existing timer=0/bits_remaining=8 initialization at rate 0,
        // the buffer empties after 2997 clocks; reload Halt is at counter 2998.
        fn automatic_overlap(start: u64, extra: u64, dmc_after_oam_reads: usize) {
            let mut nes = fixture(&[0xEA]);
            nes.bus.cpu_write(0x4015, 0x10).unwrap();
            nes.bus.apu().complete_dmc_sample_read(0xA5).unwrap();
            for _ in 0..start {
                assert_eq!(clocked_read(&mut nes, 0x8000), 0xEA);
            }
            assert_eq!(nes.cpu_cycles(), start);
            assert!(!nes.dmc_dma.is_busy());
            nes.bus.cpu_write(0x4014, 0x02).unwrap();
            clear_traces(&mut nes);

            assert_eq!(clocked_read(&mut nes, 0x8000), 0xEA);
            assert_eq!(
                nes.cpu_cycles() - start,
                514 + extra,
                "513 OAM stall + overlap extension + 1 resumed CPU read"
            );
            let reads = nes.bus.take_read_trace();
            let position = reads
                .iter()
                .position(|&a| a == 0xC081)
                .expect("APU must request and fetch the next sample DURING OAM DMA");
            assert_eq!(reads.iter().filter(|&&a| a == 0xC081).count(), 1);
            assert_eq!(
                reads[..position]
                    .iter()
                    .filter(|a| (0x0200..=0x02FF).contains(*a))
                    .count(),
                dmc_after_oam_reads
            );
            assert_eq!(
                reads
                    .iter()
                    .copied()
                    .filter(|a| (0x0200..=0x02FF).contains(a))
                    .collect::<Vec<_>>(),
                (0x0200u16..=0x02FF).collect::<Vec<_>>()
            );
            assert_eq!(reads.last(), Some(&0x8000));
            assert!(!nes.dmc_dma.is_busy());
            assert_oam_writes_and_image(&mut nes, 0x02);
        }

        #[test]
        fn automatic_dmc_reload_in_middle_of_oam_adds_two_cycles() {
            automatic_overlap(2800, 2, 100);
        }

        #[test]
        fn dmc_halt_on_second_last_oam_write_extends_transfer_by_one_cycle() {
            automatic_overlap(2488, 1, 256);
        }

        #[test]
        fn dmc_halt_on_last_oam_write_extends_transfer_by_three_cycles() {
            automatic_overlap(2486, 3, 256);
        }
    }
}
