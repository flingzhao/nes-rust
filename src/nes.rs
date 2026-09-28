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
        let cycle = self.cpu.step(&mut self.bus)?;
        for _ in 0..cycle {
            self.bus.clock_cpu_cycle();
        }

        self.cpu_cycles = self.cpu_cycles.wrapping_add(cycle as u64);
        Ok(cycle)
    }

    pub fn cpu_cycles(&self) -> u64 {
        self.cpu_cycles
    }

    pub fn bus(&self) -> &Bus {
        &self.bus
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
}
