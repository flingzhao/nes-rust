use std::ops::Sub;

use crate::bus::Bus;

const FLAG_C: u8 = 0x01; // Carry
const FLAG_Z: u8 = 0x02; // Zero
const FLAG_I: u8 = 0x04; // Interrupt Disable
const FLAG_D: u8 = 0x08; // Decimal
const FLAG_U: u8 = 0x20; // 内部表示约定
const FLAG_V: u8 = 0x40; // Overflow
const FLAG_N: u8 = 0x80; // Negative

#[derive(Debug)]
pub struct Cpu {
    a: u8,
    x: u8,
    y: u8,
    sp: u8,
    pc: u16,
    status: u8,
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0,
            pc: 0,
            status: FLAG_U | FLAG_I,
        }
    }

    pub fn flag(&self, mask: u8) -> bool {
        return self.status & mask != 0;
    }

    pub fn set_flag(&mut self, mask: u8, enabled: bool) {
        if enabled {
            self.status |= mask;
        } else {
            self.status &= mask ^ 0xff;
        }
    }

    pub fn update_zero_negative(&mut self, value: u8) {
        let zero = value == 0;
        self.set_flag(FLAG_Z, zero);

        let is_negative = value & 0x80 != 0;
        self.set_flag(FLAG_N, is_negative);
    }

    pub fn reset(&mut self, bus: &mut Bus) -> Result<(), String> {
        let low = bus.cpu_read(0xFFFC)?;
        let high = bus.cpu_read(0xFFFD)?;
        self.pc = u16::from_le_bytes([low, high]);
        self.sp = self.sp.wrapping_sub(3);

        self.set_flag(FLAG_I, true);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::Cartridge;

    fn bus_with_reset_vector(address: u16) -> Bus {
        let mut bytes = vec![0u8; 16 + 16 * 1024];

        bytes[..4].copy_from_slice(b"NES\x1A");
        bytes[4] = 1; // 16 KiB PRG ROM

        // 16 KiB 镜像映射下，FFFC 对应 ROM 偏移 3FFC。
        let index = 16 + 0x3FFC;
        bytes[index..index + 2].copy_from_slice(&address.to_le_bytes());

        let cartridge = Cartridge::try_from(bytes.as_slice()).unwrap();

        let mut bus = Bus::new();
        bus.insert_cartridge(cartridge);
        bus
    }

    #[test]
    fn reset_reads_vector_and_updates_stack_pointer() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_reset_vector(0x8234);

        cpu.reset(&mut bus).unwrap();

        assert_eq!(cpu.pc, 0x8234);
        assert_eq!(cpu.sp, 0xFD);
        assert!(cpu.flag(FLAG_I));
    }

    #[test]
    fn reset_preserves_registers_and_other_flags() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_reset_vector(0x9000);

        cpu.a = 0x11;
        cpu.x = 0x22;
        cpu.y = 0x33;
        cpu.sp = 0x80;
        cpu.status = FLAG_U | FLAG_C | FLAG_D;

        cpu.reset(&mut bus).unwrap();

        assert_eq!((cpu.a, cpu.x, cpu.y), (0x11, 0x22, 0x33));
        assert_eq!(cpu.sp, 0x7D);
        assert_eq!(cpu.status, FLAG_U | FLAG_C | FLAG_D | FLAG_I);
    }

    #[test]
    fn zero_negative_updates_preserve_other_flags() {
        let mut cpu = Cpu::new();
        cpu.set_flag(FLAG_C, true);

        for (value, zero, negative) in [
            (0x00, true, false),
            (0x80, false, true),
            (0x7F, false, false),
        ] {
            cpu.update_zero_negative(value);

            println!("{}", value);
            assert_eq!(cpu.flag(FLAG_Z), zero);
            assert_eq!(cpu.flag(FLAG_N), negative);
            assert!(cpu.flag(FLAG_C));
            assert!(cpu.flag(FLAG_I));
        }
    }

    #[test]
    fn failed_reset_does_not_change_cpu_state() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();

        cpu.pc = 0x1234;
        cpu.sp = 0x80;
        cpu.status = FLAG_U | FLAG_C;

        assert!(cpu.reset(&mut bus).is_err());

        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.sp, 0x80);
        assert_eq!(cpu.status, FLAG_U | FLAG_C);
    }
}
