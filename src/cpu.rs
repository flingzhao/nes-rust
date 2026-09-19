use crate::bus::Bus;

const FLAG_C: u8 = 0x01; // Carry
const FLAG_Z: u8 = 0x02; // Zero
const FLAG_I: u8 = 0x04; // Interrupt Disable
const FLAG_D: u8 = 0x08; // Decimal
const FLAG_U: u8 = 0x20; // 内部表示约定
const FLAG_V: u8 = 0x40; // Overflow
const FLAG_N: u8 = 0x80; // Negative

const BASE_STACKPAGE: u16 = 0x0100; // 栈底指针

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

    fn fetch_byte(&mut self, bus: &mut Bus) -> Result<u8, String> {
        let value = bus.cpu_read(self.pc)?;
        self.pc = self.pc.wrapping_add(1);
        Ok(value)
    }

    fn fetch_word(&mut self, bus: &mut Bus) -> Result<u16, String> {
        let low = self.fetch_byte(bus)?;
        let high = self.fetch_byte(bus)?;
        Ok(u16::from_le_bytes([low, high]))
    }

    fn branch(&mut self, bus: &mut Bus, condition: bool) -> Result<u8, String> {
        let offset = self.fetch_byte(bus)? as i8;
        if !condition {
            return Ok(2);
        }
        let base = self.pc;
        let target = base.wrapping_add_signed(i16::from(offset));

        let crossed_page = (base & 0xFF00) != (target & 0xFF00);
        self.pc = target;

        if !crossed_page {
            return Ok(3);
        } else {
            return Ok(4);
        }
    }

    fn push_byte(&mut self, bus: &mut Bus, value: u8) -> Result<(), String> {
        let addr = u16::from(self.sp) | BASE_STACKPAGE;
        bus.cpu_write(addr, value)?;
        self.sp = self.sp.wrapping_sub(1u8);
        Ok(())
    }

    fn pop_byte(&mut self, bus: &mut Bus) -> Result<u8, String> {
        let next_sp = self.sp.wrapping_add(1);
        let addr = u16::from(next_sp) | BASE_STACKPAGE;
        let value = bus.cpu_read(addr)?;

        self.sp = next_sp;
        Ok(value)
    }

    pub fn step(&mut self, bus: &mut Bus) -> Result<u8, String> {
        let instruction_pc = self.pc;
        let opcode = self.fetch_byte(bus)?;
        match opcode {
            0x85 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                bus.cpu_write(addr, self.a)?;
                Ok(3)
            }
            0x8D => {
                let addr = self.fetch_word(bus)?;
                bus.cpu_write(addr, self.a)?;
                Ok(4)
            }
            0xA5 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                self.a = bus.cpu_read(addr)?;
                self.update_zero_negative(self.a);
                Ok(3)
            }
            0xA9 => {
                let value = self.fetch_byte(bus)?;
                self.a = value;
                self.update_zero_negative(value);
                Ok(2)
            }
            0xAA => {
                self.x = self.a;
                self.update_zero_negative(self.x);
                Ok(2)
            }
            0xAD => {
                let addr = self.fetch_word(bus)?;
                self.a = bus.cpu_read(addr)?;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0xE8 => {
                self.x = self.x.wrapping_add(1);
                self.update_zero_negative(self.x);
                Ok(2)
            }
            0xD0 => {
                let condition = !self.flag(FLAG_Z);
                self.branch(bus, condition)
            }
            0xF0 => {
                let condition = self.flag(FLAG_Z);
                self.branch(bus, condition)
            }
            0x4C => {
                // fetch_word 取得目标地址。
                // 将 PC 设置为目标地址，返回 3 个周期。
                let target_addr = self.fetch_word(bus)?;
                self.pc = target_addr;
                Ok(3)
            }
            0x48 => {
                self.push_byte(bus, self.a)?;
                Ok(3)
            }
            0x68 => {
                self.a = self.pop_byte(bus)?;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x20 => {
                let dst = self.fetch_word(bus)?;
                let ret_addr = self.pc.wrapping_sub(1);

                let [low, high] = ret_addr.to_le_bytes();
                self.push_byte(bus, high)?;
                self.push_byte(bus, low)?;
                self.pc = dst;
                Ok(6)
            }
            0x60 => {
                let low = self.pop_byte(bus)?;
                let high = self.pop_byte(bus)?;
                self.pc = u16::from_le_bytes([low, high]).wrapping_add(1);
                Ok(6)
            }
            0xEA => Ok(2),
            _ => Err(format!(
                "Unsupported opcode {opcode:#04X} at {instruction_pc:#06X}"
            )),
        }
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

    fn bus_with_ram_program(program: &[u8]) -> Bus {
        assert!(program.len() <= 0x800);

        let mut bus = Bus::new();

        for (address, &byte) in program.iter().enumerate() {
            bus.cpu_write(address as u16, byte).unwrap();
        }

        bus
    }

    #[test]
    fn executes_first_program() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0xA9, 0xFF, // LDA #$FF
            0xAA, // TAX
            0xE8, // INX
            0xEA, // NOP
        ]);

        cpu.pc = 0;
        cpu.set_flag(FLAG_C, true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.a, 0xFF);
        assert_eq!(cpu.pc, 2);
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_Z));

        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.x, 0xFF);
        assert_eq!(cpu.pc, 3);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.x, 0);
        assert_eq!(cpu.pc, 4);
        assert!(cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
        assert!(cpu.flag(FLAG_C));

        let before = (cpu.a, cpu.x, cpu.y, cpu.sp, cpu.status);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.pc, 5);
        assert_eq!((cpu.a, cpu.x, cpu.y, cpu.sp, cpu.status), before);
    }

    #[test]
    fn lda_updates_zero_and_negative_flags() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[0xA9, 0x80, 0xA9, 0x00, 0xA9, 0x7F]);

        cpu.pc = 0;

        for (value, zero, negative) in [
            (0x80, false, true),
            (0x00, true, false),
            (0x7F, false, false),
        ] {
            cpu.step(&mut bus).unwrap();

            assert_eq!(cpu.a, value);
            assert_eq!(cpu.flag(FLAG_Z), zero);
            assert_eq!(cpu.flag(FLAG_N), negative);
        }
    }

    #[test]
    fn reports_unsupported_opcode_and_its_address() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[0x02]);

        cpu.pc = 0;

        let error = cpu.step(&mut bus).unwrap_err();

        assert!(error.contains("0x02"));
        assert!(error.contains("0x0000"));
    }

    #[test]
    fn failed_fetch_does_not_advance_pc() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();

        cpu.pc = 0x8000; // 未插卡
        assert!(cpu.fetch_byte(&mut bus).is_err());
        assert_eq!(cpu.pc, 0x8000);
    }

    #[test]
    fn loads_and_stores_through_zero_page_and_absolute_addresses() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0xA9, 0x80, // LDA #$80
            0x85, 0x40, // STA $40
            0xA9, 0x00, // LDA #$00
            0xA5, 0x40, // LDA $40
            0x8D, 0x23, 0x01, // STA $0123
            0xA9, 0x00, // LDA #$00
            0xAD, 0x23, 0x01, // LDA $0123
        ]);

        cpu.pc = 0;

        let expected = [
            // 周期数、执行后的 PC、A
            (2u8, 2u16, 0x80u8),
            (3, 4, 0x80),
            (2, 6, 0x00),
            (3, 8, 0x80),
            (4, 11, 0x80),
            (2, 13, 0x00),
            (4, 16, 0x80),
        ];

        for (cycles, pc, a) in expected {
            assert_eq!(cpu.step(&mut bus).unwrap(), cycles);
            assert_eq!(cpu.pc, pc);
            assert_eq!(cpu.a, a);
            assert_eq!(cpu.flag(FLAG_Z), a == 0);
            assert_eq!(cpu.flag(FLAG_N), a & 0x80 != 0);
        }

        assert_eq!(bus.cpu_read(0x0040).unwrap(), 0x80);
        assert_eq!(bus.cpu_read(0x0123).unwrap(), 0x80);
    }

    #[test]
    fn stores_preserve_accumulator_and_all_status_flags() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0x85, 0x40, // STA $40
            0x8D, 0x23, 0x01, // STA $0123
        ]);

        cpu.pc = 0;
        cpu.a = 0x80;
        cpu.status = FLAG_U | FLAG_C | FLAG_Z | FLAG_I | FLAG_V;
        let original_status = cpu.status;

        for cycles in [3, 4] {
            assert_eq!(cpu.step(&mut bus).unwrap(), cycles);
            assert_eq!(cpu.a, 0x80);
            assert_eq!(cpu.status, original_status);
        }

        assert_eq!(bus.cpu_read(0x0040).unwrap(), 0x80);
        assert_eq!(bus.cpu_read(0x0123).unwrap(), 0x80);
    }

    #[test]
    fn zero_page_lda_reads_address_00ff() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0xA5, 0xFF, // LDA $FF
        ]);

        cpu.pc = 0;
        bus.cpu_write(0x00FF, 0x80).unwrap();

        assert_eq!(cpu.step(&mut bus).unwrap(), 3);

        assert_eq!(cpu.a, 0x80);
        assert_eq!(cpu.pc, 2);
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_Z));
    }

    #[test]
    fn absolute_sta_propagates_unsupported_address_error() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0x8D, 0x00, 0x20, // STA $2000
        ]);

        cpu.pc = 0;
        cpu.a = 0x42;
        let original_status = cpu.status;

        assert!(cpu.step(&mut bus).is_err());

        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.status, original_status);

        // 操作码和两个地址字节已经读取，写入失败不会回滚 PC。
        assert_eq!(cpu.pc, 3);
    }

    #[test]
    fn executes_loop_until_x_wraps_to_zero() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0xA9, 0xFE, // 0000: LDA #$FE
            0xAA, // 0002: TAX
            0xE8, // 0003: INX
            0xD0, 0xFD, // 0004: BNE -3 → 0003
            0xEA, // 0006: NOP
        ]);

        cpu.pc = 0;

        // 周期数、执行后的 PC、X
        let expected = [
            (2u8, 2u16, 0x00u8), // LDA
            (2, 3, 0xFE),        // TAX
            (2, 4, 0xFF),        // INX
            (3, 3, 0xFF),        // BNE：跳转
            (2, 4, 0x00),        // INX：回绕
            (2, 6, 0x00),        // BNE：不跳转
            (2, 7, 0x00),        // NOP
        ];

        for (cycles, pc, x) in expected {
            assert_eq!(cpu.step(&mut bus).unwrap(), cycles);
            assert_eq!(cpu.pc, pc);
            assert_eq!(cpu.x, x);
        }

        assert_eq!(cpu.a, 0xFE);
        assert!(cpu.flag(FLAG_Z));
    }

    #[test]
    fn beq_uses_post_operand_pc_and_correct_cycles() {
        let cases = [
            // 起始 PC、偏移、Z、目标 PC、周期数
            (0x0000u16, 0x03u8, false, 0x0002u16, 2u8),
            (0x0000, 0x03, true, 0x0005, 3),
            (0x00FD, 0x01, true, 0x0100, 4),
            (0x0100, 0xFD, true, 0x00FF, 4),
            // 指令跨页，但分支基准和目标都在 0x01xx。
            (0x00FE, 0x01, true, 0x0101, 3),
        ];

        for (start, offset, zero, target, cycles) in cases {
            let mut cpu = Cpu::new();
            let mut bus = Bus::new();

            bus.cpu_write(start, 0xF0).unwrap();
            bus.cpu_write(start + 1, offset).unwrap();

            cpu.pc = start;
            cpu.set_flag(FLAG_Z, zero);
            let original_status = cpu.status;

            assert_eq!(cpu.step(&mut bus).unwrap(), cycles);
            assert_eq!(cpu.pc, target);
            assert_eq!(cpu.status, original_status);
        }
    }

    #[test]
    fn absolute_jmp_skips_instructions() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0x4C, 0x05, 0x00, // JMP $0005
            0xA9, 0x11, // 应跳过
            0xA9, 0x42, // 目标指令
        ]);

        cpu.pc = 0;
        let original_status = cpu.status;

        assert_eq!(cpu.step(&mut bus).unwrap(), 3);
        assert_eq!(cpu.pc, 5);
        assert_eq!(cpu.status, original_status);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.pc, 7);
    }

    #[test]
    fn stack_wraps_within_page_one_and_preserves_lifo_order() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();
        cpu.sp = 0x00;

        cpu.push_byte(&mut bus, 0x11).unwrap();
        assert_eq!(cpu.sp, 0xFF);
        assert_eq!(bus.cpu_read(0x0100).unwrap(), 0x11);

        cpu.push_byte(&mut bus, 0x22).unwrap();
        assert_eq!(cpu.sp, 0xFE);
        assert_eq!(bus.cpu_read(0x01FF).unwrap(), 0x22);

        assert_eq!(cpu.pop_byte(&mut bus).unwrap(), 0x22);
        assert_eq!(cpu.pop_byte(&mut bus).unwrap(), 0x11);
        assert_eq!(cpu.sp, 0x00);
    }

    #[test]
    fn jsr_pushes_last_operand_address_and_rts_resumes_next_instruction() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();

        // 0234: JSR $0300
        // 0237: LDA #$42
        for (i, byte) in [0x20u8, 0x00, 0x03, 0xA9, 0x42].into_iter().enumerate() {
            bus.cpu_write(0x0234 + i as u16, byte).unwrap();
        }

        // 子程序立即返回。
        bus.cpu_write(0x0300, 0x60).unwrap(); // RTS

        cpu.pc = 0x0234;
        cpu.sp = 0xFD;

        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x0300);
        assert_eq!(cpu.sp, 0xFB);

        // JSR 必须压入 0236，而不是下一条指令的地址 0237。
        assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x02);
        assert_eq!(
            bus.cpu_read(0x01FC).unwrap(),
            0x36,
            "JSR must push the next instruction address minus one"
        );

        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x0237);
        assert_eq!(cpu.sp, 0xFD);

        // 确认返回到操作码 A9，而不是操作数 42。
        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.pc, 0x0239);
    }
}
