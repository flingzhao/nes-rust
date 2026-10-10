use crate::bus::Bus;
use crate::cpu_bus::CpuBus;

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
    nmi_line: bool,    // 上次记录的 NMI 请求状态
    nmi_pending: bool, // 是否锁存了待处理的 NMI
    irq_line: bool,    // 当前 IRQ 请求是否有效
    irq_mask_override: Option<bool>,
    skip_interrupt_poll: bool,
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
            nmi_line: false,
            nmi_pending: false,
            irq_line: false,
            irq_mask_override: None,
            skip_interrupt_poll: false,
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

    pub fn reset(&mut self, bus: &mut impl CpuBus) -> Result<(), String> {
        let low = bus.cpu_read(0xFFFC)?;
        let high = bus.cpu_read(0xFFFD)?;
        self.pc = u16::from_le_bytes([low, high]);
        self.sp = self.sp.wrapping_sub(3);

        self.set_flag(FLAG_I, true);
        self.irq_mask_override = None;
        self.skip_interrupt_poll = false;

        Ok(())
    }

    fn fetch_byte(&mut self, bus: &mut impl CpuBus) -> Result<u8, String> {
        let value = bus.cpu_read(self.pc)?;
        self.pc = self.pc.wrapping_add(1);
        Ok(value)
    }

    fn fetch_word(&mut self, bus: &mut impl CpuBus) -> Result<u16, String> {
        let low = self.fetch_byte(bus)?;
        let high = self.fetch_byte(bus)?;
        Ok(u16::from_le_bytes([low, high]))
    }

    fn branch(&mut self, bus: &mut impl CpuBus, condition: bool) -> Result<u8, String> {
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

    fn push_byte(&mut self, bus: &mut impl CpuBus, value: u8) -> Result<(), String> {
        let addr = u16::from(self.sp) | BASE_STACKPAGE;
        bus.cpu_write(addr, value)?;
        self.sp = self.sp.wrapping_sub(1u8);
        Ok(())
    }

    fn pop_byte(&mut self, bus: &mut impl CpuBus) -> Result<u8, String> {
        let next_sp = self.sp.wrapping_add(1);
        let addr = u16::from(next_sp) | BASE_STACKPAGE;
        let value = bus.cpu_read(addr)?;

        self.sp = next_sp;
        Ok(value)
    }

    fn adc(&mut self, value: u8) {
        let c = self.flag(FLAG_C) as u16;
        let old_a = self.a;
        let new_a = u16::from(self.a).wrapping_add(value as u16).wrapping_add(c);
        // 加法溢出发生在两个符号相同，但是输出符号不同
        let overflow = (!(old_a ^ value) & (old_a ^ new_a as u8) & 0x80) != 0;

        self.set_flag(FLAG_V, overflow);

        let carry_in = new_a & 0x100 != 0;
        self.set_flag(FLAG_C, carry_in);
        self.a = new_a as u8;
        self.update_zero_negative(self.a);
    }

    fn sbc(&mut self, value: u8) {
        let c = self.flag(FLAG_C) as u16;
        let old_a = self.a;
        let new_a = u16::from(self.a)
            .wrapping_sub(value as u16)
            .wrapping_sub(1 - c);

        // 有符号溢出，就是数学运算的正确结果，超出了当前有符号整数类型能表示的范围。
        // 减法溢出发生在输入异号，结果相对 old_a 变号。
        let overflow = ((old_a ^ value) & (old_a ^ new_a as u8) & 0x80) != 0;
        self.set_flag(FLAG_V, overflow);

        let carry_in = new_a & 0x100 == 0;
        self.set_flag(FLAG_C, carry_in);

        self.a = new_a as u8;
        self.update_zero_negative(self.a);
    }

    fn bit_test(&mut self, value: u8) {
        self.set_flag(FLAG_Z, (self.a & value) == 0);
        self.set_flag(FLAG_N, (value & 0x80) != 0);
        self.set_flag(FLAG_V, (value & 0x40) != 0);
    }

    fn compare_register(&mut self, value: u8, register: u8) {
        let result = register.wrapping_sub(value);
        let c = register >= value;
        self.set_flag(FLAG_C, c);
        self.update_zero_negative(result);
    }

    fn asl(&mut self, value: u8) -> u8 {
        self.set_flag(FLAG_C, value & 0x80 != 0);
        let value = value << 1;
        self.update_zero_negative(value);
        value
    }

    fn lsr(&mut self, value: u8) -> u8 {
        self.set_flag(FLAG_C, value & 0x01 != 0);
        let value = value >> 1;
        self.update_zero_negative(value);
        value
    }

    fn rol(&mut self, value: u8) -> u8 {
        let carry_in = u8::from(self.flag(FLAG_C));
        self.set_flag(FLAG_C, value & 0x80 != 0);
        let value = (value << 1) | carry_in;
        self.update_zero_negative(value);
        value
    }

    fn ror(&mut self, value: u8) -> u8 {
        let carry_in = u8::from(self.flag(FLAG_C));
        self.set_flag(FLAG_C, value & 0x01 != 0);
        let value = value >> 1 | carry_in << 7;
        self.update_zero_negative(value);
        value
    }

    fn inc(&mut self, value: u8) -> u8 {
        let value = value.wrapping_add(1);
        self.update_zero_negative(value);
        value
    }

    fn dec(&mut self, value: u8) -> u8 {
        let value = value.wrapping_sub(1);
        self.update_zero_negative(value);
        value
    }

    fn fetch_absolute_indexed(
        &mut self,
        bus: &mut impl CpuBus,
        index: u8,
    ) -> Result<(u16, bool), String> {
        let base = self.fetch_word(bus)?;
        let addr = base.wrapping_add(u16::from(index));
        let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
        Ok((addr, crossed_page))
    }

    fn read_zero_page_word(&self, bus: &mut impl CpuBus, pointer: u8) -> Result<u16, String> {
        let low = bus.cpu_read(pointer as u16)?;
        let high = bus.cpu_read(pointer.wrapping_add(1) as u16)?;
        let addr = u16::from_le_bytes([low, high]);
        Ok(addr)
    }

    fn read_jmp_indirect_target(&self, bus: &mut impl CpuBus, pointer: u16) -> Result<u16, String> {
        let low = bus.cpu_read(pointer)?;
        let high = bus.cpu_read((pointer & 0xFF00) | (pointer.wrapping_add(1) & 0x00FF))?;
        let addr = u16::from_le_bytes([low, high]);
        Ok(addr)
    }

    // 公共入口动作：压栈、设置 I、读取向量。
    // vector 只由内部传入 0xFFFA 或 0xFFFE。
    fn enter_interrupt(&mut self, bus: &mut impl CpuBus, vector: u16) -> Result<u8, String> {
        self.push_byte(bus, (self.pc >> 8) as u8)?;
        self.push_byte(bus, (self.pc & 0x00FF) as u8)?;
        let stacked_status = (self.status & !0x10) | 0x20;
        self.push_byte(bus, stacked_status)?;
        self.set_flag(FLAG_I, true);
        let low = bus.cpu_read(vector)?;
        let high = bus.cpu_read(vector.wrapping_add(1))?;
        self.pc = u16::from_le_bytes([low, high]);
        self.skip_interrupt_poll = true;
        Ok(7)
    }

    pub fn nmi(&mut self, bus: &mut impl CpuBus) -> Result<u8, String> {
        self.enter_interrupt(bus, 0xFFFA)
    }

    pub fn irq(&mut self, bus: &mut impl CpuBus) -> Result<u8, String> {
        if self.flag(FLAG_I) {
            return Ok(0);
        }
        self.enter_interrupt(bus, 0xFFFE)
    }

    pub fn set_nmi_line(&mut self, asserted: bool) {
        if !self.nmi_line && asserted {
            self.nmi_pending = true;
        }

        // 保存当前信号状态，供下一次调用比较。
        self.nmi_line = asserted;
    }

    pub fn set_irq_line(&mut self, asserted: bool) {
        self.irq_line = asserted;
    }

    pub fn step(&mut self, bus: &mut impl CpuBus) -> Result<u8, String> {
        // 取出后自动重置为 false，只跳过一次。
        let skip_poll = std::mem::take(&mut self.skip_interrupt_poll);

        let irq_masked = self
            .irq_mask_override
            .take()
            .unwrap_or_else(|| self.flag(FLAG_I));

        if !skip_poll {
            if self.nmi_pending {
                let cycles = self.nmi(bus)?;
                self.nmi_pending = false;
                return Ok(cycles);
            }

            if self.irq_line && !irq_masked {
                return self.enter_interrupt(bus, 0xFFFE);
            }
        }

        let i_before = self.flag(FLAG_I);
        let instruction_pc = self.pc;
        let opcode = self.fetch_byte(bus)?;
        let cycles = match opcode {
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
            0x18 => {
                self.set_flag(FLAG_C, false);
                Ok(2)
            }
            0x38 => {
                self.set_flag(FLAG_C, true);
                Ok(2)
            }
            0x69 => {
                let value = self.fetch_byte(bus)?;
                self.adc(value);
                Ok(2)
            }
            0xE9 => {
                let value = self.fetch_byte(bus)?;
                self.sbc(value);
                Ok(2)
            }
            0xC9 => {
                let value = self.fetch_byte(bus)?;
                self.compare_register(value, self.a);
                Ok(2)
            }
            0xE0 => {
                let value = self.fetch_byte(bus)?;
                self.compare_register(value, self.x);
                Ok(2)
            }
            0xC0 => {
                let value = self.fetch_byte(bus)?;
                self.compare_register(value, self.y);
                Ok(2)
            }
            0xA2 => {
                let value = self.fetch_byte(bus)?;
                self.x = value;
                self.update_zero_negative(value);
                Ok(2)
            }
            0xA0 => {
                let value = self.fetch_byte(bus)?;
                self.y = value;
                self.update_zero_negative(value);
                Ok(2)
            }
            0xA8 => {
                self.y = self.a;
                self.update_zero_negative(self.y);
                Ok(2)
            }
            0x8A => {
                self.a = self.x;
                self.update_zero_negative(self.a);
                Ok(2)
            }
            0x98 => {
                self.a = self.y;
                self.update_zero_negative(self.a);
                Ok(2)
            }
            0xBA => {
                self.x = self.sp;
                self.update_zero_negative(self.x);
                Ok(2)
            }
            0x9A => {
                self.sp = self.x;
                Ok(2)
            }
            0xCA => {
                self.x = self.x.wrapping_sub(1);
                self.update_zero_negative(self.x);
                Ok(2)
            }
            0xC8 => {
                self.y = self.y.wrapping_add(1);
                self.update_zero_negative(self.y);
                Ok(2)
            }
            0x88 => {
                self.y = self.y.wrapping_sub(1);
                self.update_zero_negative(self.y);
                Ok(2)
            }
            0x29 => {
                let value = self.fetch_byte(bus)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                Ok(2)
            }
            0x09 => {
                let value = self.fetch_byte(bus)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                Ok(2)
            }
            0x49 => {
                let value = self.fetch_byte(bus)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                Ok(2)
            }
            0x24 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let value = bus.cpu_read(addr)?;
                self.bit_test(value);
                Ok(3)
            }
            0x2C => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.bit_test(value);
                Ok(4)
            }
            0x0A => {
                self.a = self.asl(self.a);
                Ok(2)
            }
            0x4A => {
                self.a = self.lsr(self.a);
                Ok(2)
            }
            0x2A => {
                self.a = self.rol(self.a);
                Ok(2)
            }
            0x6A => {
                self.a = self.ror(self.a);
                Ok(2)
            }
            0x06 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.asl(old);
                bus.cpu_write(addr, new)?;
                Ok(5)
            }
            0x46 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.lsr(old);
                bus.cpu_write(addr, new)?;
                Ok(5)
            }
            0x26 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.rol(old);
                bus.cpu_write(addr, new)?;
                Ok(5)
            }
            0x66 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.ror(old);
                bus.cpu_write(addr, new)?;
                Ok(5)
            }
            0xE6 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.inc(old);
                bus.cpu_write(addr, new)?;
                Ok(5)
            }
            0xC6 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.dec(old);
                bus.cpu_write(addr, new)?;
                Ok(5)
            }
            0x0E => {
                let addr = self.fetch_word(bus)?;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.asl(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0x4E => {
                let addr = self.fetch_word(bus)?;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.lsr(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0x2E => {
                let addr = self.fetch_word(bus)?;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.rol(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0x6E => {
                let addr = self.fetch_word(bus)?;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.ror(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0xEE => {
                let addr = self.fetch_word(bus)?;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.inc(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0xCE => {
                let addr = self.fetch_word(bus)?;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.dec(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0xB5 => {
                // LDA zero page,X
                let base = self.fetch_byte(bus)?;
                let addr = u16::from(base.wrapping_add(self.x));
                self.a = bus.cpu_read(addr)?;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x95 => {
                // STA zero page,X
                let base = self.fetch_byte(bus)?;
                let addr = u16::from(base.wrapping_add(self.x));
                bus.cpu_write(addr, self.a)?;
                Ok(4)
            }
            0xB6 => {
                // LDX zero page,Y
                let base = self.fetch_byte(bus)?;
                let addr = u16::from(base.wrapping_add(self.y));
                self.x = bus.cpu_read(addr)?;
                self.update_zero_negative(self.x);
                Ok(4)
            }
            0x96 => {
                // STX zero page,Y
                let base = self.fetch_byte(bus)?;
                let addr = u16::from(base.wrapping_add(self.y));
                bus.cpu_write(addr, self.x)?;
                Ok(4)
            }
            0xB4 => {
                // LDY zero page,X
                let base = self.fetch_byte(bus)?;
                let addr = u16::from(base.wrapping_add(self.x));
                self.y = bus.cpu_read(addr)?;
                self.update_zero_negative(self.y);
                Ok(4)
            }
            0x94 => {
                // STY zero page,X
                let base = self.fetch_byte(bus)?;
                let addr = u16::from(base.wrapping_add(self.x));
                bus.cpu_write(addr, self.y)?;
                Ok(4)
            }
            0xBD => {
                let (addr, crossed_page) = self.fetch_absolute_indexed(bus, self.x)?;
                self.a = bus.cpu_read(addr)?;
                self.update_zero_negative(self.a);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0xB9 => {
                let (addr, crossed_page) = self.fetch_absolute_indexed(bus, self.y)?;
                self.a = bus.cpu_read(addr)?;
                self.update_zero_negative(self.a);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0xBE => {
                let (addr, crossed_page) = self.fetch_absolute_indexed(bus, self.y)?;
                self.x = bus.cpu_read(addr)?;
                self.update_zero_negative(self.x);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0xBC => {
                let (addr, crossed_page) = self.fetch_absolute_indexed(bus, self.x)?;
                self.y = bus.cpu_read(addr)?;
                self.update_zero_negative(self.y);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x9D => {
                let (addr, _) = self.fetch_absolute_indexed(bus, self.x)?;
                bus.cpu_write(addr, self.a)?;
                Ok(5)
            }
            0x99 => {
                let (addr, _) = self.fetch_absolute_indexed(bus, self.y)?;
                bus.cpu_write(addr, self.a)?;
                Ok(5)
            }
            0xA1 => {
                // LDA ($nn, X)
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                self.a = bus.cpu_read(addr)?;
                self.update_zero_negative(self.a);
                Ok(6)
            }
            0x81 => {
                // STA ($nn, X)
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                bus.cpu_write(addr, self.a)?;
                Ok(6)
            }
            0xB1 => {
                // LDA ($nn), Y
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                self.a = bus.cpu_read(addr)?;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 6 } else { 5 })
            }
            0x91 => {
                // STA ($nn), Y
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                bus.cpu_write(addr, self.a)?;
                Ok(6)
            }
            0xA6 => {
                let addr = self.fetch_byte(bus)? as u16;
                self.x = bus.cpu_read(addr)?;
                self.update_zero_negative(self.x);
                Ok(3)
            }
            0xA4 => {
                let addr = self.fetch_byte(bus)? as u16;
                self.y = bus.cpu_read(addr)?;
                self.update_zero_negative(self.y);
                Ok(3)
            }
            0x86 => {
                let addr = self.fetch_byte(bus)? as u16;
                bus.cpu_write(addr, self.x)?;
                Ok(3)
            }
            0x84 => {
                let addr = self.fetch_byte(bus)? as u16;
                bus.cpu_write(addr, self.y)?;
                Ok(3)
            }
            0xAE => {
                let addr = self.fetch_word(bus)?;
                self.x = bus.cpu_read(addr)?;
                self.update_zero_negative(self.x);
                Ok(4)
            }
            0xAC => {
                let addr = self.fetch_word(bus)?;
                self.y = bus.cpu_read(addr)?;
                self.update_zero_negative(self.y);
                Ok(4)
            }
            0x8E => {
                let addr = self.fetch_word(bus)?;
                bus.cpu_write(addr, self.x)?;
                Ok(4)
            }
            0x8C => {
                let addr = self.fetch_word(bus)?;
                bus.cpu_write(addr, self.y)?;
                Ok(4)
            }
            0xC5 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.a);
                Ok(3)
            }
            0xE4 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.x);
                Ok(3)
            }
            0xC4 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.y);
                Ok(3)
            }
            0xCD => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.a);
                Ok(4)
            }
            0xEC => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.x);
                Ok(4)
            }
            0xCC => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.y);
                Ok(4)
            }
            0xD5 => {
                let addr = u16::from(self.fetch_byte(bus)?.wrapping_add(self.x));
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.a);
                Ok(4)
            }
            0xDD => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.x as u16);
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0xD9 => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0xC1 => {
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.a);
                Ok(6)
            }
            0xD1 => {
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.compare_register(value, self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 6 } else { 5 })
            }
            0x65 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let value = bus.cpu_read(addr)?;
                self.adc(value);
                Ok(3)
            }
            0xE5 => {
                let addr = u16::from(self.fetch_byte(bus)?);
                let value = bus.cpu_read(addr)?;
                self.sbc(value);
                Ok(3)
            }
            0x75 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let value = bus.cpu_read(addr)?;
                self.adc(value);
                Ok(4)
            }
            0xF5 => {
                let addr = u16::from(self.fetch_byte(bus)?.wrapping_add(self.x));
                let value = bus.cpu_read(addr)?;
                self.sbc(value);
                Ok(4)
            }
            0x6D => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.adc(value);
                Ok(4)
            }
            0xED => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.sbc(value);
                Ok(4)
            }
            0x7D => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.x as u16);
                let value = bus.cpu_read(addr)?;
                self.adc(value);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x79 => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                self.adc(value);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x61 => {
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                let value = bus.cpu_read(addr)?;
                self.adc(value);
                Ok(6)
            }
            0x71 => {
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.adc(value);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 6 } else { 5 })
            }
            0xFD => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.x as u16);
                let value = bus.cpu_read(addr)?;
                self.sbc(value);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0xF9 => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                self.sbc(value);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0xE1 => {
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                let value = bus.cpu_read(addr)?;
                self.sbc(value);
                Ok(6)
            }
            0xF1 => {
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.sbc(value);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 6 } else { 5 })
            }
            0x25 => {
                let addr = self.fetch_byte(bus)? as u16;
                let value = bus.cpu_read(addr)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                Ok(3)
            }
            0x35 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let value = bus.cpu_read(addr)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x2D => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x3D => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.x as u16);
                let value = bus.cpu_read(addr)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x39 => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x21 => {
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                let value = bus.cpu_read(addr)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                Ok(6)
            }
            0x31 => {
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.a &= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 6 } else { 5 })
            }
            0x05 => {
                let addr = self.fetch_byte(bus)? as u16;
                let value = bus.cpu_read(addr)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                Ok(3)
            }
            0x15 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let value = bus.cpu_read(addr)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x0D => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x1D => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.x as u16);
                let value = bus.cpu_read(addr)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x19 => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x01 => {
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                let value = bus.cpu_read(addr)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                Ok(6)
            }
            0x11 => {
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.a |= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 6 } else { 5 })
            }
            0x45 => {
                let addr = self.fetch_byte(bus)? as u16;
                let value = bus.cpu_read(addr)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                Ok(3)
            }
            0x55 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let value = bus.cpu_read(addr)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x4D => {
                let addr = self.fetch_word(bus)?;
                let value = bus.cpu_read(addr)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                Ok(4)
            }
            0x5D => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.x as u16);
                let value = bus.cpu_read(addr)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x59 => {
                let base = self.fetch_word(bus)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 5 } else { 4 })
            }
            0x41 => {
                let pointer = self.fetch_byte(bus)?.wrapping_add(self.x);
                let addr = self.read_zero_page_word(bus, pointer)?;
                let value = bus.cpu_read(addr)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                Ok(6)
            }
            0x51 => {
                let pointer = self.fetch_byte(bus)?;
                let base = self.read_zero_page_word(bus, pointer)?;
                let addr = base.wrapping_add(self.y as u16);
                let value = bus.cpu_read(addr)?;
                self.a ^= value;
                self.update_zero_negative(self.a);
                let crossed_page = (base & 0xFF00) != (addr & 0xFF00);
                Ok(if crossed_page { 6 } else { 5 })
            }
            0x16 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.asl(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0x56 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.lsr(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0x36 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.rol(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0x76 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.ror(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0xF6 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.inc(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0xD6 => {
                let addr = self.fetch_byte(bus)?.wrapping_add(self.x) as u16;
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.dec(old);
                bus.cpu_write(addr, new)?;
                Ok(6)
            }
            0x1E => {
                let addr = self.fetch_word(bus)?.wrapping_add(self.x as u16);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.asl(old);
                bus.cpu_write(addr, new)?;
                Ok(7)
            }
            0x5E => {
                let addr = self.fetch_word(bus)?.wrapping_add(self.x as u16);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.lsr(old);
                bus.cpu_write(addr, new)?;
                Ok(7)
            }
            0x3E => {
                let addr = self.fetch_word(bus)?.wrapping_add(self.x as u16);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.rol(old);
                bus.cpu_write(addr, new)?;
                Ok(7)
            }
            0x7E => {
                let addr = self.fetch_word(bus)?.wrapping_add(self.x as u16);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.ror(old);
                bus.cpu_write(addr, new)?;
                Ok(7)
            }
            0xFE => {
                let addr = self.fetch_word(bus)?.wrapping_add(self.x as u16);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.inc(old);
                bus.cpu_write(addr, new)?;
                Ok(7)
            }
            0xDE => {
                let addr = self.fetch_word(bus)?.wrapping_add(self.x as u16);
                let old = bus.cpu_read(addr)?;
                bus.cpu_write(addr, old)?;
                let new = self.dec(old);
                bus.cpu_write(addr, new)?;
                Ok(7)
            }
            // BCC：C = 0 时跳转
            0x90 => {
                let condition = !self.flag(FLAG_C);
                self.branch(bus, condition)
            }

            // BCS：C = 1 时跳转
            0xB0 => {
                let condition = self.flag(FLAG_C);
                self.branch(bus, condition)
            }

            // BPL：N = 0 时跳转
            0x10 => {
                let condition = !self.flag(FLAG_N);
                self.branch(bus, condition)
            }

            // BMI：N = 1 时跳转
            0x30 => {
                let condition = self.flag(FLAG_N);
                self.branch(bus, condition)
            }

            // BVC：V = 0 时跳转
            0x50 => {
                let condition = !self.flag(FLAG_V);
                self.branch(bus, condition)
            }

            // BVS：V = 1 时跳转
            0x70 => {
                let condition = self.flag(FLAG_V);
                self.branch(bus, condition)
            }

            // CLD：清除十进制标志
            0xD8 => {
                self.set_flag(FLAG_D, false);
                Ok(2)
            }

            // CLI：清除 IRQ 屏蔽标志
            0x58 => {
                self.set_flag(FLAG_I, false);
                Ok(2)
            }

            // CLV：清除溢出标志
            0xB8 => {
                self.set_flag(FLAG_V, false);
                Ok(2)
            }

            // SED：设置十进制标志
            0xF8 => {
                self.set_flag(FLAG_D, true);
                Ok(2)
            }

            // SEI：设置 IRQ 屏蔽标志
            0x78 => {
                self.set_flag(FLAG_I, true);
                Ok(2)
            }
            // PHP
            0x08 => {
                // NV11DIZC
                let flags = self.status | 0x30;
                self.push_byte(bus, flags)?;
                Ok(3)
            }
            // PLP
            0x28 => {
                let flags = self.pop_byte(bus)? & 0xCF;
                self.status = flags;
                Ok(4)
            }
            // JMP
            0x6C => {
                let addr = self.fetch_word(bus)?;
                self.pc = self.read_jmp_indirect_target(bus, addr)?;
                Ok(5)
            }
            // BRK
            0x00 => {
                let _ = self.fetch_byte(bus)?;
                self.push_byte(bus, (self.pc >> 8) as u8)?;
                self.push_byte(bus, (self.pc & 0x00FF) as u8)?;
                self.push_byte(bus, self.status | 0x30)?;
                self.set_flag(FLAG_I, true);
                let low = bus.cpu_read(0xFFFE)?;
                let high = bus.cpu_read(0xFFFF)?;
                self.pc = u16::from_le_bytes([low, high]);
                self.skip_interrupt_poll = true;
                Ok(7)
            }
            // RTI
            0x40 => {
                self.status = self.pop_byte(bus)? & 0xCF;
                let low = self.pop_byte(bus)?;
                let high = self.pop_byte(bus)?;
                self.pc = u16::from_le_bytes([low, high]);
                Ok(6)
            }
            0xEA => Ok(2),
            _ => Err(format!(
                "Unsupported opcode {opcode:#04X} at {instruction_pc:#06X}"
            )),
        }?;

        if matches!(opcode, 0x58 | 0x78 | 0x28) {
            self.irq_mask_override = Some(i_before);
        }

        Ok(cycles)
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

    fn write_irq_test_program(bus: &mut impl CpuBus, program: &[u8]) {
        for (offset, &byte) in program.iter().enumerate() {
            bus.cpu_write(0x0200 + offset as u16, byte).unwrap();
        }
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

    #[test]
    fn nested_calls_restore_each_return_address() {
        let mut cpu = Cpu::new();
        let mut bus = Bus::new();

        let programs: [(u16, &[u8]); 3] = [
            (
                0x0200,
                &[
                    0x20, 0x00, 0x03, // JSR $0300
                    0xEA, // NOP
                ],
            ),
            (
                0x0300,
                &[
                    0x20, 0x00, 0x04, // JSR $0400
                    0x60, // RTS
                ],
            ),
            (
                0x0400,
                &[
                    0xA9, 0x42, // LDA #$42
                    0x60, // RTS
                ],
            ),
        ];

        for (start, program) in programs {
            for (offset, &byte) in program.iter().enumerate() {
                bus.cpu_write(start + offset as u16, byte).unwrap();
            }
        }

        cpu.pc = 0x0200;
        cpu.sp = 0xFD;

        // 外层 JSR：保存 0202。
        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x0300);
        assert_eq!(cpu.sp, 0xFB);
        assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x02);
        assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x02);

        // 内层 JSR：保存 0302，不覆盖外层返回位置。
        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x0400);
        assert_eq!(cpu.sp, 0xF9);
        assert_eq!(bus.cpu_read(0x01FB).unwrap(), 0x03);
        assert_eq!(bus.cpu_read(0x01FA).unwrap(), 0x02);
        assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x02);
        assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x02);

        let expectations = [
            // 周期数、执行后的 PC、SP
            (2u8, 0x0402u16, 0xF9u8), // LDA
            (6, 0x0303, 0xFB),        // 内层 RTS
            (6, 0x0203, 0xFD),        // 外层 RTS
            (2, 0x0204, 0xFD),        // 主程序 NOP
        ];

        for (index, (cycles, pc, sp)) in expectations.into_iter().enumerate() {
            assert_eq!(
                cpu.step(&mut bus).unwrap(),
                cycles,
                "Cycle mismatch at remaining step {index}"
            );
            assert_eq!(cpu.pc, pc, "PC mismatch at step {index}");
            assert_eq!(cpu.sp, sp, "SP mismatch at step {index}");
        }

        assert_eq!(cpu.a, 0x42);
    }

    #[test]
    fn immediate_adc_sbc_update_arithmetic_flags() {
        let cases = [
            // opcode, A, operand, carry_in, result, C, V
            (0x69u8, 0x01u8, 0x01u8, false, 0x02u8, false, false),
            (0x69, 0x01, 0x01, true, 0x03, false, false),
            (0x69, 0xFF, 0x01, false, 0x00, true, false),
            (0x69, 0x7F, 0x01, false, 0x80, false, true),
            (0x69, 0x80, 0x80, false, 0x00, true, true),
            (0x69, 0x09, 0x01, false, 0x0A, false, false),
            (0xE9, 0x05, 0x03, true, 0x02, true, false),
            (0xE9, 0x05, 0x03, false, 0x01, true, false),
            (0xE9, 0x00, 0x01, true, 0xFF, false, false),
            (0xE9, 0x00, 0x00, true, 0x00, true, false),
            (0xE9, 0x00, 0x00, false, 0xFF, false, false),
            (0xE9, 0x80, 0x01, true, 0x7F, true, true),
            (0xE9, 0x7F, 0xFF, true, 0x80, false, true),
        ];

        for (index, (opcode, a, operand, carry_in, result, c, v)) in cases.into_iter().enumerate() {
            // D=0 和 D=1 都应得到相同的二进制运算结果。
            for decimal in [false, true] {
                let mut cpu = Cpu::new();
                let mut bus = bus_with_ram_program(&[opcode, operand]);

                cpu.pc = 0;
                cpu.a = a;
                cpu.status = FLAG_U | FLAG_I | FLAG_V | FLAG_Z | FLAG_N;
                cpu.set_flag(FLAG_C, carry_in);
                cpu.set_flag(FLAG_D, decimal);

                assert_eq!(cpu.step(&mut bus).unwrap(), 2);

                assert_eq!(cpu.a, result, "A mismatch in case {index}");
                assert_eq!(cpu.pc, 2);
                assert_eq!(cpu.flag(FLAG_C), c, "C mismatch in case {index}");
                assert_eq!(cpu.flag(FLAG_V), v, "V mismatch in case {index}");
                assert_eq!(cpu.flag(FLAG_Z), result == 0);
                assert_eq!(cpu.flag(FLAG_N), result & 0x80 != 0);

                // 运算不应破坏其他标志。
                let preserved_mask = !(FLAG_C | FLAG_V | FLAG_Z | FLAG_N);
                let expected_preserved = FLAG_U | FLAG_I | if decimal { FLAG_D } else { 0 };

                assert_eq!(cpu.status & preserved_mask, expected_preserved);
            }
        }
    }

    #[test]
    fn immediate_comparisons_update_only_c_z_n() {
        let cases = [
            // register, operand, C, Z, N
            (0x05u8, 0x03u8, true, false, false),
            (0x03, 0x05, false, false, true),
            (0x42, 0x42, true, true, false),
            (0x00, 0xFF, false, false, false),
            (0xFF, 0x00, true, false, true),
            (0x80, 0x01, true, false, false),
        ];

        for opcode in [0xC9u8, 0xE0, 0xC0] {
            for (register, operand, c, z, n) in cases {
                // 验证比较不依赖旧 C，也不改变旧 V。
                for initial_c in [false, true] {
                    for initial_v in [false, true] {
                        let mut cpu = Cpu::new();
                        let mut bus = bus_with_ram_program(&[opcode, operand]);

                        cpu.pc = 0;
                        cpu.a = 0x11;
                        cpu.x = 0x22;
                        cpu.y = 0x33;
                        cpu.sp = 0xFD;

                        match opcode {
                            0xC9 => cpu.a = register,
                            0xE0 => cpu.x = register,
                            0xC0 => cpu.y = register,
                            _ => unreachable!(),
                        }

                        cpu.status = FLAG_U | FLAG_I | FLAG_D;
                        cpu.set_flag(FLAG_C, initial_c);
                        cpu.set_flag(FLAG_V, initial_v);

                        // 刻意设成与预期相反，检查设置和清除。
                        cpu.set_flag(FLAG_Z, !z);
                        cpu.set_flag(FLAG_N, !n);

                        let before_registers = (cpu.a, cpu.x, cpu.y, cpu.sp);
                        let before_status = cpu.status;

                        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
                        assert_eq!(cpu.pc, 2);
                        assert_eq!((cpu.a, cpu.x, cpu.y, cpu.sp), before_registers);

                        let changed_mask = FLAG_C | FLAG_Z | FLAG_N;
                        let expected_status = (before_status & !changed_mask)
                            | if c { FLAG_C } else { 0 }
                            | if z { FLAG_Z } else { 0 }
                            | if n { FLAG_N } else { 0 };

                        assert_eq!(
                            cpu.status, expected_status,
                            "opcode={opcode:#04X}, \
                         register={register:#04X}, \
                         operand={operand:#04X}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn immediate_index_loads_update_only_target_and_z_n() {
        for opcode in [0xA2u8, 0xA0] {
            for value in [0x00u8, 0x7F, 0x80, 0xFF] {
                let mut cpu = Cpu::new();
                let mut bus = bus_with_ram_program(&[opcode, value]);

                cpu.pc = 0;
                cpu.a = 0x11;
                cpu.x = 0x22;
                cpu.y = 0x33;
                cpu.sp = 0xFD;
                cpu.status = FLAG_U | FLAG_I | FLAG_D | FLAG_C | FLAG_V | FLAG_Z | FLAG_N;

                let preserved = cpu.status & !(FLAG_Z | FLAG_N);

                assert_eq!(cpu.step(&mut bus).unwrap(), 2);
                assert_eq!(cpu.pc, 2);
                assert_eq!(cpu.a, 0x11);
                assert_eq!(cpu.sp, 0xFD);

                if opcode == 0xA2 {
                    assert_eq!((cpu.x, cpu.y), (value, 0x33));
                } else {
                    assert_eq!((cpu.x, cpu.y), (0x22, value));
                }

                assert_eq!(cpu.flag(FLAG_Z), value == 0);
                assert_eq!(cpu.flag(FLAG_N), value & 0x80 != 0);
                assert_eq!(cpu.status & !(FLAG_Z | FLAG_N), preserved);
            }
        }
    }

    #[test]
    fn txs_preserves_flags_but_tsx_updates_zero_negative() {
        for value in [0x00u8, 0x80, 0x7F] {
            let mut cpu = Cpu::new();
            let mut bus = bus_with_ram_program(&[0x9A, 0xBA]);

            cpu.pc = 0;
            cpu.x = value;
            cpu.sp = 0xFD;
            cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V;

            // 让旧 Z、N 与 value 对应的结果相反。
            cpu.set_flag(FLAG_Z, value != 0);
            cpu.set_flag(FLAG_N, value & 0x80 == 0);
            let before = cpu.status;

            assert_eq!(cpu.step(&mut bus).unwrap(), 2); // TXS
            assert_eq!(cpu.sp, value);
            assert_eq!(cpu.x, value);
            assert_eq!(cpu.status, before);

            cpu.x = 0x55; // 确保 TSX 确实从 SP 读取。

            assert_eq!(cpu.step(&mut bus).unwrap(), 2); // TSX
            assert_eq!(cpu.x, value);
            assert_eq!(cpu.sp, value);
            assert_eq!(cpu.pc, 2);

            assert_eq!(cpu.flag(FLAG_Z), value == 0);
            assert_eq!(cpu.flag(FLAG_N), value & 0x80 != 0);
            assert_eq!(cpu.status & !(FLAG_Z | FLAG_N), before & !(FLAG_Z | FLAG_N));
        }
    }

    #[test]
    fn register_increment_decrement_wrap_and_preserve_other_flags() {
        let cases = [
            // opcode, initial, expected
            (0xCAu8, 0x00u8, 0xFFu8), // DEX
            (0xCA, 0x01, 0x00),
            (0xC8, 0xFF, 0x00), // INY
            (0xC8, 0x7F, 0x80),
            (0x88, 0x00, 0xFF), // DEY
            (0x88, 0x01, 0x00),
        ];

        for (opcode, initial, expected) in cases {
            for initial_cv in [0u8, FLAG_C | FLAG_V] {
                let mut cpu = Cpu::new();
                let mut bus = bus_with_ram_program(&[opcode]);

                cpu.pc = 0;
                cpu.a = 0x11;
                cpu.x = 0x22;
                cpu.y = 0x33;
                cpu.sp = 0xFD;
                cpu.status = FLAG_U | FLAG_I | FLAG_D | initial_cv | FLAG_Z | FLAG_N;

                let preserved = cpu.status & !(FLAG_Z | FLAG_N);

                if opcode == 0xCA {
                    cpu.x = initial;
                } else {
                    cpu.y = initial;
                }

                assert_eq!(cpu.step(&mut bus).unwrap(), 2);
                assert_eq!(cpu.pc, 1);

                if opcode == 0xCA {
                    assert_eq!((cpu.x, cpu.y), (expected, 0x33));
                } else {
                    assert_eq!((cpu.x, cpu.y), (0x22, expected));
                }

                assert_eq!(cpu.a, 0x11);
                assert_eq!(cpu.sp, 0xFD);
                assert_eq!(cpu.flag(FLAG_Z), expected == 0);
                assert_eq!(cpu.flag(FLAG_N), expected & 0x80 != 0);
                assert_eq!(cpu.status & !(FLAG_Z | FLAG_N), preserved);
            }
        }
    }

    #[test]
    fn register_transfer_chain_preserves_sources_and_updates_flags() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0xA2, 0x80, // LDX #$80
            0x8A, // TXA
            0xA8, // TAY
            0xA0, 0x00, // LDY #$00
            0x98, // TYA
        ]);

        cpu.pc = 0;
        cpu.a = 0;
        cpu.x = 0;
        cpu.y = 0;
        cpu.sp = 0xFD;
        cpu.status = FLAG_U | FLAG_I | FLAG_D | FLAG_C | FLAG_V;

        let preserved_flags = cpu.status;
        let mut total_cycles = 0u32;

        let expectations = [
            // PC, A, X, Y, Z, N
            (2u16, 0x00u8, 0x80u8, 0x00u8, false, true),
            (3, 0x80, 0x80, 0x00, false, true),
            (4, 0x80, 0x80, 0x80, false, true),
            (6, 0x80, 0x80, 0x00, true, false),
            (7, 0x00, 0x80, 0x00, true, false),
        ];

        for (index, (pc, a, x, y, z, n)) in expectations.into_iter().enumerate() {
            let cycles = cpu.step(&mut bus).unwrap();
            total_cycles += u32::from(cycles);

            assert_eq!(cycles, 2, "Cycles at step {index}");
            assert_eq!(cpu.pc, pc, "PC at step {index}");
            assert_eq!(
                (cpu.a, cpu.x, cpu.y),
                (a, x, y),
                "Registers at step {index}"
            );
            assert_eq!(cpu.sp, 0xFD);

            assert_eq!(cpu.flag(FLAG_Z), z, "Z at step {index}");
            assert_eq!(cpu.flag(FLAG_N), n, "N at step {index}");
            assert_eq!(
                cpu.status & !(FLAG_Z | FLAG_N),
                preserved_flags,
                "Other flags at step {index}"
            );
        }

        assert_eq!(total_cycles, 10);
    }

    #[test]
    fn immediate_logic_updates_a_and_only_z_n() {
        let cases = [
            // opcode, A, operand, result
            (0x29u8, 0xF0u8, 0x0Fu8, 0x00u8), // AND
            (0x29, 0xFF, 0x80, 0x80),
            (0x09, 0x00, 0x00, 0x00), // ORA
            (0x09, 0x40, 0x80, 0xC0),
            (0x49, 0xAA, 0xAA, 0x00), // EOR
            (0x49, 0x55, 0xFF, 0xAA),
            (0x49, 0xFF, 0x80, 0x7F),
        ];

        for (opcode, a, operand, expected) in cases {
            for initial_cv in [0u8, FLAG_C | FLAG_V] {
                let mut cpu = Cpu::new();
                let mut bus = bus_with_ram_program(&[opcode, operand]);

                cpu.pc = 0;
                cpu.a = a;
                cpu.x = 0x12;
                cpu.y = 0x34;
                cpu.sp = 0xFD;
                cpu.status = FLAG_U | FLAG_I | FLAG_D | initial_cv;

                // 检查 Z、N 既能置位，也能清除。
                cpu.set_flag(FLAG_Z, expected != 0);
                cpu.set_flag(FLAG_N, expected & 0x80 == 0);
                let preserved = cpu.status & !(FLAG_Z | FLAG_N);

                assert_eq!(cpu.step(&mut bus).unwrap(), 2);
                assert_eq!(cpu.pc, 2);
                assert_eq!(cpu.a, expected);
                assert_eq!((cpu.x, cpu.y, cpu.sp), (0x12, 0x34, 0xFD));

                assert_eq!(cpu.flag(FLAG_Z), expected == 0);
                assert_eq!(cpu.flag(FLAG_N), expected & 0x80 != 0);
                assert_eq!(cpu.status & !(FLAG_Z | FLAG_N), preserved);
            }
        }
    }

    #[test]
    fn bit_reads_n_v_from_memory_and_z_from_and_result() {
        let cases = [
            // A, memory, Z, N, V
            (0x01u8, 0xC0u8, true, true, true),
            (0x80, 0x80, false, true, false),
            (0x40, 0x40, false, false, true),
            (0xFF, 0x00, true, false, false),
        ];

        let modes: [(&[u8], u16, u16, u8); 2] = [
            // 程序字节、数据地址、最终 PC、周期数
            (&[0x24, 0x40], 0x0040, 2, 3),
            (&[0x2C, 0x23, 0x01], 0x0123, 3, 4),
        ];

        for (program, addr, expected_pc, cycles) in modes {
            for (a, memory, z, n, v) in cases {
                for carry in [false, true] {
                    let mut cpu = Cpu::new();
                    let mut bus = bus_with_ram_program(program);
                    bus.cpu_write(addr, memory).unwrap();

                    cpu.pc = 0;
                    cpu.a = a;
                    cpu.x = 0x12;
                    cpu.y = 0x34;
                    cpu.sp = 0xFD;
                    cpu.status = FLAG_U | FLAG_I | FLAG_D;

                    cpu.set_flag(FLAG_C, carry);
                    cpu.set_flag(FLAG_Z, !z);
                    cpu.set_flag(FLAG_N, !n);
                    cpu.set_flag(FLAG_V, !v);

                    let changed = FLAG_Z | FLAG_N | FLAG_V;
                    let preserved = cpu.status & !changed;

                    assert_eq!(cpu.step(&mut bus).unwrap(), cycles);
                    assert_eq!(cpu.pc, expected_pc);
                    assert_eq!((cpu.a, cpu.x, cpu.y, cpu.sp), (a, 0x12, 0x34, 0xFD));
                    assert_eq!(bus.cpu_read(addr).unwrap(), memory);

                    assert_eq!(cpu.flag(FLAG_Z), z);
                    assert_eq!(cpu.flag(FLAG_N), n);
                    assert_eq!(cpu.flag(FLAG_V), v);
                    assert_eq!(cpu.status & !changed, preserved);
                }
            }
        }
    }

    #[test]
    fn logic_chain_updates_a_and_preserves_other_flags() {
        for carry in [false, true] {
            for overflow in [false, true] {
                let mut cpu = Cpu::new();
                let mut bus = bus_with_ram_program(&[
                    0x29, 0x0F, // AND #$0F
                    0x09, 0x80, // ORA #$80
                    0x49, 0xFF, // EOR #$FF
                ]);

                cpu.pc = 0;
                cpu.a = 0xA5;
                cpu.x = 0x12;
                cpu.y = 0x34;
                cpu.sp = 0xFD;
                cpu.status = FLAG_U | FLAG_I | FLAG_D | FLAG_Z | FLAG_N;
                cpu.set_flag(FLAG_C, carry);
                cpu.set_flag(FLAG_V, overflow);

                let preserved_flags = cpu.status & !(FLAG_Z | FLAG_N);
                let mut total_cycles = 0u32;

                let expectations = [
                    // PC, A, Z, N
                    (2u16, 0x05u8, false, false),
                    (4, 0x85, false, true),
                    (6, 0x7A, false, false),
                ];

                for (index, (pc, a, z, n)) in expectations.into_iter().enumerate() {
                    let cycles = cpu.step(&mut bus).unwrap();
                    total_cycles += u32::from(cycles);

                    assert_eq!(cycles, 2, "Cycles at step {index}");
                    assert_eq!(cpu.pc, pc, "PC at step {index}");
                    assert_eq!(cpu.a, a, "A at step {index}");
                    assert_eq!(cpu.flag(FLAG_Z), z, "Z at step {index}");
                    assert_eq!(cpu.flag(FLAG_N), n, "N at step {index}");

                    assert_eq!(
                        cpu.status & !(FLAG_Z | FLAG_N),
                        preserved_flags,
                        "Other flags at step {index}"
                    );
                    assert_eq!((cpu.x, cpu.y, cpu.sp), (0x12, 0x34, 0xFD));
                }

                assert_eq!(total_cycles, 6);
            }
        }
    }

    #[test]
    fn accumulator_shifts_and_rotates_update_c_z_n() {
        let cases = [
            // opcode, A, old C, expected A, expected C

            // ASL：忽略输入 Carry。
            (0x0Au8, 0x80u8, false, 0x00u8, true),
            (0x0A, 0x80, true, 0x00, true),
            (0x0A, 0x40, true, 0x80, false),
            (0x0A, 0x01, false, 0x02, false),
            // LSR：忽略输入 Carry，结果最高位为零。
            (0x4A, 0x01, false, 0x00, true),
            (0x4A, 0x01, true, 0x00, true),
            (0x4A, 0x80, true, 0x40, false),
            (0x4A, 0xFF, false, 0x7F, true),
            // ROL：必须使用旧 Carry。
            (0x2A, 0x80, false, 0x00, true),
            (0x2A, 0x80, true, 0x01, true),
            (0x2A, 0x00, true, 0x01, false),
            (0x2A, 0x40, false, 0x80, false),
            // ROR：旧 Carry 进入 bit 7。
            (0x6A, 0x01, false, 0x00, true),
            (0x6A, 0x01, true, 0x80, true),
            (0x6A, 0x00, true, 0x80, false),
            (0x6A, 0x80, false, 0x40, false),
        ];

        for (opcode, a, carry_in, expected_a, expected_c) in cases {
            for overflow in [false, true] {
                let mut cpu = Cpu::new();
                let mut bus = bus_with_ram_program(&[opcode]);

                cpu.pc = 0;
                cpu.a = a;
                cpu.x = 0x12;
                cpu.y = 0x34;
                cpu.sp = 0xFD;
                cpu.status = FLAG_U | FLAG_I | FLAG_D;

                cpu.set_flag(FLAG_C, carry_in);
                cpu.set_flag(FLAG_V, overflow);
                cpu.set_flag(FLAG_Z, expected_a != 0);
                cpu.set_flag(FLAG_N, expected_a & 0x80 == 0);

                let changed = FLAG_C | FLAG_Z | FLAG_N;
                let preserved = cpu.status & !changed;

                assert_eq!(cpu.step(&mut bus).unwrap(), 2);
                assert_eq!(cpu.pc, 1);
                assert_eq!(
                    cpu.a, expected_a,
                    "opcode={opcode:#04X}, A={a:#04X}, C={carry_in}"
                );
                assert_eq!(cpu.flag(FLAG_C), expected_c);
                assert_eq!(cpu.flag(FLAG_Z), expected_a == 0);
                assert_eq!(cpu.flag(FLAG_N), expected_a & 0x80 != 0);
                assert_eq!(cpu.status & !changed, preserved);
                assert_eq!((cpu.x, cpu.y, cpu.sp), (0x12, 0x34, 0xFD));
            }
        }
    }

    #[test]
    fn nine_rol_instructions_restore_accumulator_and_carry() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[0x2A; 9]);

        cpu.pc = 0;
        cpu.a = 0xA5;
        cpu.x = 0x12;
        cpu.y = 0x34;
        cpu.sp = 0xFD;
        cpu.status = FLAG_U | FLAG_I | FLAG_D | FLAG_V | FLAG_C;

        let preserved = cpu.status & !(FLAG_C | FLAG_Z | FLAG_N);
        let mut total_cycles = 0u32;

        let expectations = [
            // 每次执行后的 A、C
            (0x4Bu8, true),
            (0x97, false),
            (0x2E, true),
            (0x5D, false),
            (0xBA, false),
            (0x74, true),
            (0xE9, false),
            (0xD2, true),
            (0xA5, true),
        ];

        for (index, (a, carry)) in expectations.into_iter().enumerate() {
            let cycles = cpu.step(&mut bus).unwrap();
            total_cycles += u32::from(cycles);

            assert_eq!(cycles, 2);
            assert_eq!(cpu.pc, (index + 1) as u16);
            assert_eq!(cpu.a, a, "A at step {index}");
            assert_eq!(cpu.flag(FLAG_C), carry, "C at step {index}");
            assert_eq!(cpu.flag(FLAG_Z), a == 0);
            assert_eq!(cpu.flag(FLAG_N), a & 0x80 != 0);
            assert_eq!(cpu.status & !(FLAG_C | FLAG_Z | FLAG_N), preserved);
            assert_eq!((cpu.x, cpu.y, cpu.sp), (0x12, 0x34, 0xFD));
        }

        assert_eq!(cpu.a, 0xA5);
        assert!(cpu.flag(FLAG_C));
        assert_eq!(cpu.pc, 9);
        assert_eq!(total_cycles, 18);
    }

    #[test]
    fn memory_rmw_updates_data_without_changing_registers() {
        let cases = [
            // 零页码、绝对码、原值、输入 C、结果、输出 C
            (0x06u8, 0x0Eu8, 0x80u8, false, 0x00u8, true), // ASL
            (0x06, 0x0E, 0x40, true, 0x80, false),
            (0x46, 0x4E, 0x01, false, 0x00, true), // LSR
            (0x46, 0x4E, 0x80, true, 0x40, false),
            (0x26, 0x2E, 0x80, true, 0x01, true), // ROL
            (0x26, 0x2E, 0x00, true, 0x01, false),
            (0x66, 0x6E, 0x01, true, 0x80, true), // ROR
            (0x66, 0x6E, 0x00, true, 0x80, false),
            (0xE6, 0xEE, 0xFF, false, 0x00, false), // INC
            (0xE6, 0xEE, 0x7F, true, 0x80, true),
            (0xC6, 0xCE, 0x00, false, 0xFF, false), // DEC
            (0xC6, 0xCE, 0x01, true, 0x00, true),
        ];

        for (zp, abs, old, carry_in, expected, carry_out) in cases {
            for absolute in [false, true] {
                for overflow in [false, true] {
                    let (program, addr, pc, cycles) = if absolute {
                        (vec![abs, 0x23, 0x01], 0x0123u16, 3u16, 6u8)
                    } else {
                        (vec![zp, 0x40], 0x0040u16, 2u16, 5u8)
                    };

                    let mut cpu = Cpu::new();
                    let mut bus = bus_with_ram_program(&program);

                    bus.cpu_write(addr, old).unwrap();
                    bus.cpu_write(addr - 1, 0x5A).unwrap();
                    bus.cpu_write(addr + 1, 0xA5).unwrap();

                    cpu.pc = 0;
                    cpu.a = 0x11;
                    cpu.x = 0x22;
                    cpu.y = 0x33;
                    cpu.sp = 0xFD;
                    cpu.status = FLAG_U | FLAG_I | FLAG_D;
                    cpu.set_flag(FLAG_C, carry_in);
                    cpu.set_flag(FLAG_V, overflow);
                    cpu.set_flag(FLAG_Z, expected != 0);
                    cpu.set_flag(FLAG_N, expected & 0x80 == 0);

                    let preserved = cpu.status & !(FLAG_C | FLAG_Z | FLAG_N);

                    assert_eq!(cpu.step(&mut bus).unwrap(), cycles);
                    assert_eq!(cpu.pc, pc);
                    assert_eq!(bus.cpu_read(addr).unwrap(), expected);
                    assert_eq!(bus.cpu_read(addr - 1).unwrap(), 0x5A);
                    assert_eq!(bus.cpu_read(addr + 1).unwrap(), 0xA5);

                    assert_eq!((cpu.a, cpu.x, cpu.y, cpu.sp), (0x11, 0x22, 0x33, 0xFD));
                    assert_eq!(cpu.flag(FLAG_C), carry_out);
                    assert_eq!(cpu.flag(FLAG_Z), expected == 0);
                    assert_eq!(cpu.flag(FLAG_N), expected & 0x80 != 0);
                    assert_eq!(cpu.status & !(FLAG_C | FLAG_Z | FLAG_N), preserved);
                }
            }
        }
    }
    #[test]
    fn inc_then_ror_uses_carry_preserved_by_inc() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0xE6, 0x40, // INC $40
            0x66, 0x40, // ROR $40
        ]);

        bus.cpu_write(0x0040, 0xFF).unwrap();

        cpu.pc = 0;
        cpu.a = 0x42;
        cpu.status = FLAG_U | FLAG_I | FLAG_V | FLAG_C;

        assert_eq!(cpu.step(&mut bus).unwrap(), 5);
        assert_eq!(cpu.pc, 2);
        assert_eq!(bus.cpu_read(0x0040).unwrap(), 0x00);
        assert!(cpu.flag(FLAG_C), "INC 不应修改 Carry");
        assert!(cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));

        assert_eq!(cpu.step(&mut bus).unwrap(), 5);
        assert_eq!(cpu.pc, 4);
        assert_eq!(bus.cpu_read(0x0040).unwrap(), 0x80);
        assert!(!cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_N));

        assert_eq!(cpu.a, 0x42);
        assert!(cpu.flag(FLAG_V));
    }

    #[test]
    fn indexed_zero_page_loads_wrap_and_use_the_right_index() {
        let cases = [
            // opcode, operand, A, X, Y, expected A, X, Y
            (
                0xB5u8, 0xFEu8, 0x11u8, 0x42u8, 0x43u8, 0x80u8, 0x42u8, 0x43u8,
            ), // LDA $FE,X
            (0xB6, 0xFD, 0x11, 0x42, 0x43, 0x11, 0x80, 0x43), // LDX $FD,Y
            (0xB4, 0xFE, 0x11, 0x42, 0x43, 0x11, 0x42, 0x80), // LDY $FE,X
        ];

        for (opcode, operand, a, x, y, expected_a, expected_x, expected_y) in cases {
            let mut cpu = Cpu::new();
            let mut bus = bus_with_ram_program(&[opcode, operand]);

            bus.cpu_write(0x0040, 0x80).unwrap();
            bus.cpu_write(0x0140, 0x77).unwrap();

            cpu.pc = 0;
            cpu.a = a;
            cpu.x = x;
            cpu.y = y;
            cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V;

            assert_eq!(cpu.step(&mut bus).unwrap(), 4);
            assert_eq!(cpu.pc, 2);
            assert_eq!((cpu.a, cpu.x, cpu.y), (expected_a, expected_x, expected_y));
            assert_eq!(bus.cpu_read(0x0040).unwrap(), 0x80);
            assert_eq!(bus.cpu_read(0x0140).unwrap(), 0x77);
            assert!(cpu.flag(FLAG_N));
            assert!(!cpu.flag(FLAG_Z));
            assert!(cpu.flag(FLAG_C));
            assert!(cpu.flag(FLAG_V));
        }
    }

    #[test]
    fn indexed_zero_page_stores_wrap_without_changing_flags() {
        let cases = [
            // opcode, operand, expected memory value
            (0x95u8, 0xFEu8, 0x55u8), // STA $FE,X
            (0x96, 0xFD, 0x42),       // STX $FD,Y
            (0x94, 0xFE, 0x43),       // STY $FE,X
        ];

        for (opcode, operand, expected) in cases {
            let mut cpu = Cpu::new();
            let mut bus = bus_with_ram_program(&[opcode, operand]);

            bus.cpu_write(0x0040, 0x80).unwrap();
            bus.cpu_write(0x0140, 0x77).unwrap();

            cpu.pc = 0;
            cpu.a = 0x55;
            cpu.x = 0x42;
            cpu.y = 0x43;
            cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V | FLAG_Z;
            let before_status = cpu.status;

            assert_eq!(cpu.step(&mut bus).unwrap(), 4);
            assert_eq!(cpu.pc, 2);
            assert_eq!(bus.cpu_read(0x0040).unwrap(), expected);
            assert_eq!(bus.cpu_read(0x0140).unwrap(), 0x77);
            assert_eq!((cpu.a, cpu.x, cpu.y), (0x55, 0x42, 0x43));
            assert_eq!(cpu.status, before_status);
        }
    }

    #[test]
    fn indexed_zero_page_load_without_wrap() {
        let mut cpu = Cpu::new();
        let mut bus = bus_with_ram_program(&[
            0xB5, 0x20, // LDA $20,X
        ]);

        bus.cpu_write(0x0023, 0x7F).unwrap();

        cpu.pc = 0;
        cpu.x = 0x03;

        assert_eq!(cpu.step(&mut bus).unwrap(), 4);
        assert_eq!(cpu.a, 0x7F);
        assert_eq!(cpu.pc, 2);
        assert!(!cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
    }

    #[test]
    fn absolute_indexed_lda_reads_target_and_counts_page_crossing() {
        let cases = [
            // opcode, base, index, target, cycles
            (0xBDu8, 0x0120u16, 3u8, 0x0123u16, 4u8), // ,X 不跨页
            (0xB9, 0x0120, 3, 0x0123, 4),             // ,Y 不跨页
            (0xBD, 0x01FF, 1, 0x0200, 5),             // ,X 跨页
            (0xB9, 0x01FF, 1, 0x0200, 5),             // ,Y 跨页
        ];

        for (opcode, base, index, target, cycles) in cases {
            let [low, high] = base.to_le_bytes();
            let mut bus = bus_with_ram_program(&[opcode, low, high]);
            bus.cpu_write(target, 0x80).unwrap();

            let mut cpu = Cpu::new();
            cpu.pc = 0;
            cpu.a = 0x11;
            cpu.x = if opcode == 0xBD { index } else { 0x10 };
            cpu.y = if opcode == 0xB9 { index } else { 0x10 };
            cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V;

            assert_eq!(cpu.step(&mut bus).unwrap(), cycles);
            assert_eq!(cpu.pc, 3);
            assert_eq!(cpu.a, 0x80);
            assert!(cpu.flag(FLAG_N));
            assert!(!cpu.flag(FLAG_Z));
            assert!(cpu.flag(FLAG_C));
            assert!(cpu.flag(FLAG_V));
            assert_eq!(bus.cpu_read(target).unwrap(), 0x80);
        }
    }

    #[test]
    fn absolute_indexed_sta_always_takes_five_cycles() {
        let cases = [
            // opcode, base, index, target
            (0x9Du8, 0x0120u16, 3u8, 0x0123u16),
            (0x99, 0x0120, 3, 0x0123),
            (0x9D, 0x01FF, 1, 0x0200),
            (0x99, 0x01FF, 1, 0x0200),
        ];

        for (opcode, base, index, target) in cases {
            let [low, high] = base.to_le_bytes();
            let mut bus = bus_with_ram_program(&[opcode, low, high]);

            let mut cpu = Cpu::new();
            cpu.pc = 0;
            cpu.a = 0x5A;
            cpu.x = if opcode == 0x9D { index } else { 0x10 };
            cpu.y = if opcode == 0x99 { index } else { 0x10 };
            cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V | FLAG_Z;
            let original_status = cpu.status;

            assert_eq!(cpu.step(&mut bus).unwrap(), 5);
            assert_eq!(cpu.pc, 3);
            assert_eq!(cpu.a, 0x5A);
            assert_eq!(cpu.status, original_status);
            assert_eq!(bus.cpu_read(target).unwrap(), 0x5A);
        }
    }

    #[test]
    fn absolute_indexed_address_wraps_at_ffff() {
        let mut bus = Bus::new();

        // 程序放在 0200，避免与目标地址 0000 重叠。
        for (i, byte) in [0xBDu8, 0xFF, 0xFF].into_iter().enumerate() {
            bus.cpu_write(0x0200 + i as u16, byte).unwrap();
        }
        bus.cpu_write(0x0000, 0x42).unwrap();

        let mut cpu = Cpu::new();
        cpu.pc = 0x0200;
        cpu.x = 1;

        assert_eq!(cpu.step(&mut bus).unwrap(), 5);
        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.pc, 0x0203);
    }

    #[test]
    fn indexed_indirect_x_wraps_pointer_within_zero_page() {
        for opcode in [0xA1u8, 0x81] {
            let mut bus = Bus::new();

            bus.cpu_write(0x0300, opcode).unwrap();
            bus.cpu_write(0x0301, 0xFE).unwrap();

            // FE + X(1) = FF；指针高字节位于 00，而非 0100。
            bus.cpu_write(0x00FF, 0x00).unwrap();
            bus.cpu_write(0x0000, 0x04).unwrap();
            bus.cpu_write(0x0400, 0x80).unwrap();

            let mut cpu = Cpu::new();
            cpu.pc = 0x0300;
            cpu.x = 1;
            cpu.y = 0x20;
            cpu.a = if opcode == 0x81 { 0x55 } else { 0x11 };
            cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V;
            let before_status = cpu.status;

            assert_eq!(cpu.step(&mut bus).unwrap(), 6);
            assert_eq!(cpu.pc, 0x0302);
            assert_eq!((cpu.x, cpu.y), (1, 0x20));

            if opcode == 0xA1 {
                assert_eq!(cpu.a, 0x80);
                assert_eq!(bus.cpu_read(0x0400).unwrap(), 0x80);
                assert!(cpu.flag(FLAG_N));
                assert!(!cpu.flag(FLAG_Z));
                assert!(cpu.flag(FLAG_C));
                assert!(cpu.flag(FLAG_V));
            } else {
                assert_eq!(cpu.a, 0x55);
                assert_eq!(bus.cpu_read(0x0400).unwrap(), 0x55);
                assert_eq!(cpu.status, before_status);
            }
        }
    }

    #[test]
    fn indirect_indexed_y_counts_crossing_only_for_loads() {
        let cases = [
            // 指针指向的基础地址、Y、最终地址、LDA 周期
            (0x03FEu16, 1u8, 0x03FFu16, 5u8),
            (0x03FF, 1, 0x0400, 6),
        ];

        for (base, y, target, load_cycles) in cases {
            for opcode in [0xB1u8, 0x91] {
                let mut bus = Bus::new();

                bus.cpu_write(0x0300, opcode).unwrap();
                bus.cpu_write(0x0301, 0xFF).unwrap();

                // 指针从零页 FF 跨回 00。
                let [low, high] = base.to_le_bytes();
                bus.cpu_write(0x00FF, low).unwrap();
                bus.cpu_write(0x0000, high).unwrap();
                bus.cpu_write(target, 0x80).unwrap();

                let mut cpu = Cpu::new();
                cpu.pc = 0x0300;
                cpu.x = 0x20;
                cpu.y = y;
                cpu.a = if opcode == 0x91 { 0x55 } else { 0x11 };
                cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V;
                let before_status = cpu.status;

                let expected_cycles = if opcode == 0xB1 { load_cycles } else { 6 };

                assert_eq!(cpu.step(&mut bus).unwrap(), expected_cycles);
                assert_eq!(cpu.pc, 0x0302);
                assert_eq!((cpu.x, cpu.y), (0x20, y));

                if opcode == 0xB1 {
                    assert_eq!(cpu.a, 0x80);
                    assert_eq!(bus.cpu_read(target).unwrap(), 0x80);
                    assert!(cpu.flag(FLAG_N));
                    assert!(!cpu.flag(FLAG_Z));
                    assert!(cpu.flag(FLAG_C));
                    assert!(cpu.flag(FLAG_V));
                } else {
                    assert_eq!(cpu.a, 0x55);
                    assert_eq!(bus.cpu_read(target).unwrap(), 0x55);
                    assert_eq!(cpu.status, before_status);
                }
            }
        }
    }

    #[test]
    fn indirect_indexed_y_wraps_final_sixteen_bit_address() {
        let mut bus = Bus::new();

        bus.cpu_write(0x0300, 0xB1).unwrap(); // LDA ($40),Y
        bus.cpu_write(0x0301, 0x40).unwrap();
        bus.cpu_write(0x0040, 0xFF).unwrap();
        bus.cpu_write(0x0041, 0xFF).unwrap();
        bus.cpu_write(0x0000, 0x42).unwrap();

        let mut cpu = Cpu::new();
        cpu.pc = 0x0300;
        cpu.y = 1;

        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.a, 0x42);
        assert_eq!(cpu.pc, 0x0302);
    }

    #[test]
    fn index_register_memory_loads_and_stores() {
        let cases = [
            // opcode, absolute, load, use_x
            (0xA6u8, false, true, true), // LDX zero page
            (0xAE, true, true, true),    // LDX absolute
            (0xA4, false, true, false),  // LDY zero page
            (0xAC, true, true, false),   // LDY absolute
            (0x86, false, false, true),  // STX zero page
            (0x8E, true, false, true),   // STX absolute
            (0x84, false, false, false), // STY zero page
            (0x8C, true, false, false),  // STY absolute
        ];

        for (opcode, absolute, load, use_x) in cases {
            for memory_value in [0x00u8, 0x80] {
                let program = if absolute {
                    vec![opcode, 0x40, 0x01]
                } else {
                    vec![opcode, 0x40]
                };

                let addr = if absolute { 0x0140 } else { 0x0040 };
                let mut bus = bus_with_ram_program(&program);
                bus.cpu_write(addr, memory_value).unwrap();

                let mut cpu = Cpu::new();
                cpu.pc = 0;
                cpu.a = 0x33;
                cpu.x = 0x11;
                cpu.y = 0x22;
                cpu.sp = 0xFD;
                cpu.status = FLAG_U | FLAG_I | FLAG_C | FLAG_V;

                // 加载测试中让旧 Z、N 与预期相反。
                cpu.set_flag(FLAG_Z, memory_value != 0);
                cpu.set_flag(FLAG_N, memory_value & 0x80 == 0);
                let original_status = cpu.status;

                let expected_cycles = if absolute { 4 } else { 3 };
                assert_eq!(cpu.step(&mut bus).unwrap(), expected_cycles);
                assert_eq!(cpu.pc, if absolute { 3 } else { 2 });
                assert_eq!((cpu.a, cpu.sp), (0x33, 0xFD));

                if load {
                    let expected = if use_x {
                        (memory_value, 0x22)
                    } else {
                        (0x11, memory_value)
                    };

                    assert_eq!((cpu.x, cpu.y), expected);
                    assert_eq!(bus.cpu_read(addr).unwrap(), memory_value);
                    assert_eq!(cpu.flag(FLAG_Z), memory_value == 0);
                    assert_eq!(cpu.flag(FLAG_N), memory_value & 0x80 != 0);
                    assert_eq!(
                        cpu.status & !(FLAG_Z | FLAG_N),
                        original_status & !(FLAG_Z | FLAG_N)
                    );
                } else {
                    assert_eq!((cpu.x, cpu.y), (0x11, 0x22));
                    assert_eq!(bus.cpu_read(addr).unwrap(), if use_x { 0x11 } else { 0x22 });
                    assert_eq!(cpu.status, original_status);
                }
            }
        }
    }
    #[test]
    fn memory_compare_instructions_update_only_czn() {
        // opcode, 是否绝对寻址, 比较的寄存器, 周期
        let instructions = [
            (0xC5, false, 'A', 3),
            (0xCD, true, 'A', 4),
            (0xE4, false, 'X', 3),
            (0xEC, true, 'X', 4),
            (0xC4, false, 'Y', 3),
            (0xCC, true, 'Y', 4),
        ];

        // 寄存器值, 内存值, 预期 C, Z, N
        let cases = [
            (0x05, 0x03, true, false, false),
            (0x05, 0x05, true, true, false),
            (0x03, 0x05, false, false, true),
            (0x00, 0xFF, false, false, false), // 00 - FF = 01（按 u8 回绕）
            (0x80, 0x01, true, false, false),
        ];

        for (opcode, absolute, register, cycles) in instructions {
            for (reg_value, memory_value, c, z, n) in cases {
                let mut bus = Bus::new();
                let mut cpu = Cpu::new();
                cpu.pc = 0;

                cpu.a = 0x11;
                cpu.x = 0x22;
                cpu.y = 0x33;
                match register {
                    'A' => cpu.a = reg_value,
                    'X' => cpu.x = reg_value,
                    'Y' => cpu.y = reg_value,
                    _ => unreachable!(),
                }
                let registers_before = (cpu.a, cpu.x, cpu.y);

                // 预置 V，验证比较指令不会清掉它。
                cpu.status = FLAG_V;

                let data_addr = if absolute { 0x0140 } else { 0x0040 };
                bus.cpu_write(0x0000, opcode).unwrap();
                bus.cpu_write(0x0001, 0x40).unwrap();
                if absolute {
                    bus.cpu_write(0x0002, 0x01).unwrap();
                }
                bus.cpu_write(data_addr, memory_value).unwrap();

                assert_eq!(cpu.step(&mut bus).unwrap(), cycles, "opcode {opcode:#04X}");
                assert_eq!(cpu.pc, if absolute { 3 } else { 2 });
                assert_eq!((cpu.a, cpu.x, cpu.y), registers_before);
                assert_eq!(bus.cpu_read(data_addr).unwrap(), memory_value);

                assert_eq!(cpu.flag(FLAG_C), c, "opcode {opcode:#04X}");
                assert_eq!(cpu.flag(FLAG_Z), z, "opcode {opcode:#04X}");
                assert_eq!(cpu.flag(FLAG_N), n, "opcode {opcode:#04X}");
                assert!(cpu.flag(FLAG_V), "opcode {opcode:#04X} changed V");
            }
        }
    }
    #[test]
    fn cmp_indexed_addressing_and_cycles() {
        fn check(
            opcode: u8,
            operands: &[u8],
            x: u8,
            y: u8,
            target: u16,
            value: u8,
            pointer_bytes: &[(u16, u8)],
            expected_cycles: u8,
            expected_czn: (bool, bool, bool),
        ) {
            let mut bus = Bus::new();
            let mut cpu = Cpu::new();

            // 程序不放在零页，避免与间接寻址使用的指针冲突。
            cpu.pc = 0x0300;
            cpu.a = 0x42;
            cpu.x = x;
            cpu.y = y;
            cpu.status = FLAG_V;

            bus.cpu_write(0x0300, opcode).unwrap();
            for (index, byte) in operands.iter().copied().enumerate() {
                bus.cpu_write(0x0301 + index as u16, byte).unwrap();
            }
            for &(addr, byte) in pointer_bytes {
                bus.cpu_write(addr, byte).unwrap();
            }
            bus.cpu_write(target, value).unwrap();

            assert_eq!(
                cpu.step(&mut bus).unwrap(),
                expected_cycles,
                "opcode {opcode:#04X}"
            );
            assert_eq!(cpu.pc, 0x0301 + operands.len() as u16);
            assert_eq!((cpu.a, cpu.x, cpu.y), (0x42, x, y));
            assert_eq!(bus.cpu_read(target).unwrap(), value);

            let (c, z, n) = expected_czn;
            assert_eq!(cpu.flag(FLAG_C), c, "opcode {opcode:#04X}");
            assert_eq!(cpu.flag(FLAG_Z), z, "opcode {opcode:#04X}");
            assert_eq!(cpu.flag(FLAG_N), n, "opcode {opcode:#04X}");
            assert!(cpu.flag(FLAG_V), "opcode {opcode:#04X} changed V");
        }

        // 零页 $F0 + X=$20 回绕到 $10。
        check(
            0xD5,
            &[0xF0],
            0x20,
            0,
            0x0010,
            0x42,
            &[],
            4,
            (true, true, false),
        );

        // 绝对,X：跨页与不跨页。
        check(
            0xDD,
            &[0xFF, 0x01],
            1,
            0,
            0x0200,
            0x50,
            &[],
            5,
            (false, false, true),
        );
        check(
            0xDD,
            &[0x40, 0x01],
            1,
            0,
            0x0141,
            0x40,
            &[],
            4,
            (true, false, false),
        );

        // 绝对,Y：跨页与不跨页。
        check(
            0xD9,
            &[0xFF, 0x01],
            0,
            1,
            0x0200,
            0x42,
            &[],
            5,
            (true, true, false),
        );
        check(
            0xD9,
            &[0x40, 0x01],
            0,
            1,
            0x0141,
            0x50,
            &[],
            4,
            (false, false, true),
        );

        // ($FE,X)：X=2 后指针地址回绕到零页 $00。
        check(
            0xC1,
            &[0xFE],
            2,
            0,
            0x0140,
            0x40,
            &[(0x0000, 0x40), (0x0001, 0x01)],
            6,
            (true, false, false),
        );

        // ($FF),Y：指针高字节从零页 $00 读取；测试跨页与不跨页。
        check(
            0xD1,
            &[0xFF],
            0,
            1,
            0x0200,
            0x50,
            &[(0x00FF, 0xFF), (0x0000, 0x01)],
            6,
            (false, false, true),
        );
        check(
            0xD1,
            &[0xFF],
            0,
            1,
            0x0141,
            0x42,
            &[(0x00FF, 0x40), (0x0000, 0x01)],
            5,
            (true, true, false),
        );
    }
    #[test]
    fn adc_sbc_memory_modes() {
        fn check(
            opcode: u8,
            operands: &[u8],
            data_addr: u16,
            x: u8,
            initial_a: u8,
            value: u8,
            carry_in: bool,
            expected: (u8, bool, bool, bool, bool, u8),
        ) {
            let mut bus = Bus::new();
            let mut cpu = Cpu::new();
            cpu.pc = 0x0300;
            cpu.a = initial_a;
            cpu.x = x;
            cpu.set_flag(FLAG_C, carry_in);

            bus.cpu_write(0x0300, opcode).unwrap();
            for (i, byte) in operands.iter().copied().enumerate() {
                bus.cpu_write(0x0301 + i as u16, byte).unwrap();
            }
            bus.cpu_write(data_addr, value).unwrap();

            let (a, c, z, v, n, cycles) = expected;
            assert_eq!(cpu.step(&mut bus).unwrap(), cycles, "opcode {opcode:#04X}");
            assert_eq!(cpu.pc, 0x0301 + operands.len() as u16);
            assert_eq!(cpu.a, a, "opcode {opcode:#04X}");
            assert_eq!(bus.cpu_read(data_addr).unwrap(), value);
            assert_eq!(cpu.flag(FLAG_C), c);
            assert_eq!(cpu.flag(FLAG_Z), z);
            assert_eq!(cpu.flag(FLAG_V), v);
            assert_eq!(cpu.flag(FLAG_N), n);
        }

        // ADC $40：正数 + 正数产生有符号溢出。
        check(
            0x65,
            &[0x40],
            0x0040,
            0,
            0x7F,
            0x01,
            false,
            (0x80, false, false, true, true, 3),
        );

        // ADC $F0,X：X=$20，零页地址回绕到 $10。
        check(
            0x75,
            &[0xF0],
            0x0010,
            0x20,
            0xFF,
            0x00,
            true,
            (0x00, true, true, false, false, 4),
        );

        // ADC $0140，验证输入进位参与计算。
        check(
            0x6D,
            &[0x40, 0x01],
            0x0140,
            0,
            0x10,
            0x20,
            true,
            (0x31, false, false, false, false, 4),
        );

        // SBC $40：C=1 表示没有额外借位。
        check(
            0xE5,
            &[0x40],
            0x0040,
            0,
            0x00,
            0x01,
            true,
            (0xFF, false, false, false, true, 3),
        );

        // SBC $F0,X：有符号溢出，同时不发生无符号借位。
        check(
            0xF5,
            &[0xF0],
            0x0010,
            0x20,
            0x80,
            0x01,
            true,
            (0x7F, true, false, true, false, 4),
        );

        // SBC $0140：结果为零。
        check(
            0xED,
            &[0x40, 0x01],
            0x0140,
            0,
            0x10,
            0x10,
            true,
            (0x00, true, true, false, false, 4),
        );

        // C=0 时 SBC 还要额外减 1。
        check(
            0xED,
            &[0x40, 0x01],
            0x0140,
            0,
            0x10,
            0x10,
            false,
            (0xFF, false, false, false, true, 4),
        );

        check(
            0xE5,
            &[0x40],
            0x0040,
            0x20,
            0x00,
            0x01,
            true,
            (0xFF, false, false, false, true, 3),
        );
    }

    #[test]
    fn adc_sbc_remaining_addressing_modes() {
        fn check(
            opcode: u8,
            operands: &[u8],
            x: u8,
            y: u8,
            pointer_bytes: &[(u16, u8)],
            target: u16,
            expected_cycles: u8,
        ) {
            let mut bus = Bus::new();
            let mut cpu = Cpu::new();
            let is_sbc = matches!(opcode, 0xFD | 0xF9 | 0xE1 | 0xF1);

            cpu.pc = 0x0300;
            cpu.x = x;
            cpu.y = y;
            cpu.a = if is_sbc { 0x80 } else { 0x7F };
            cpu.set_flag(FLAG_C, is_sbc);

            bus.cpu_write(0x0300, opcode).unwrap();
            for (i, byte) in operands.iter().copied().enumerate() {
                bus.cpu_write(0x0301 + i as u16, byte).unwrap();
            }
            for &(addr, byte) in pointer_bytes {
                bus.cpu_write(addr, byte).unwrap();
            }
            bus.cpu_write(target, 0x01).unwrap();

            assert_eq!(
                cpu.step(&mut bus).unwrap(),
                expected_cycles,
                "opcode {opcode:#04X}"
            );
            assert_eq!(cpu.pc, 0x0301 + operands.len() as u16);
            assert_eq!(cpu.a, if is_sbc { 0x7F } else { 0x80 });
            assert_eq!(bus.cpu_read(target).unwrap(), 0x01);
            assert_eq!(cpu.flag(FLAG_C), is_sbc);
            assert!(!cpu.flag(FLAG_Z));
            assert!(cpu.flag(FLAG_V));
            assert_eq!(cpu.flag(FLAG_N), !is_sbc);
        }

        for opcode in [0x7D, 0xFD] {
            check(opcode, &[0x40, 0x01], 1, 0, &[], 0x0141, 4);
            check(opcode, &[0xFF, 0x01], 1, 0, &[], 0x0200, 5);
        }

        for opcode in [0x79, 0xF9] {
            check(opcode, &[0x40, 0x01], 0, 1, &[], 0x0141, 4);
            check(opcode, &[0xFF, 0x01], 0, 1, &[], 0x0200, 5);
        }

        // $FE + X=2 回绕到零页 $00，再从 $00/$01 读出 $0140。
        for opcode in [0x61, 0xE1] {
            check(
                opcode,
                &[0xFE],
                2,
                0,
                &[(0x0000, 0x40), (0x0001, 0x01)],
                0x0140,
                6,
            );
        }

        // 指针从零页 $FF/$00 读取；Y 分别导致不跨页、跨页。
        for opcode in [0x71, 0xF1] {
            check(
                opcode,
                &[0xFF],
                0,
                1,
                &[(0x00FF, 0x40), (0x0000, 0x01)],
                0x0141,
                5,
            );
            check(
                opcode,
                &[0xFF],
                0,
                1,
                &[(0x00FF, 0xFF), (0x0000, 0x01)],
                0x0200,
                6,
            );
        }
    }

    #[test]
    fn logical_memory_modes() {
        // 顺序：零页、零页X、绝对、绝对X、绝对Y、(间接,X)、(间接),Y
        let opcode_groups = [
            [0x25, 0x35, 0x2D, 0x3D, 0x39, 0x21, 0x31], // AND
            [0x05, 0x15, 0x0D, 0x1D, 0x19, 0x01, 0x11], // ORA
            [0x45, 0x55, 0x4D, 0x5D, 0x59, 0x41, 0x51], // EOR
        ];

        type Scenario = (
            usize,         // 上表中的寻址方式序号
            &'static [u8], // 指令操作数字节
            u8,
            u8,                   // X, Y
            &'static [(u16, u8)], // 零页指针的预置字节
            u16,                  // 最终读取地址
            u8,                   // 预期周期
        );

        let scenarios: [Scenario; 10] = [
            (0, &[0x40], 0, 0, &[], 0x0040, 3),
            (1, &[0xF0], 0x20, 0, &[], 0x0010, 4), // 零页回绕
            (2, &[0x40, 0x01], 0, 0, &[], 0x0140, 4),
            (3, &[0x40, 0x01], 1, 0, &[], 0x0141, 4),
            (3, &[0xFF, 0x01], 1, 0, &[], 0x0200, 5),
            (4, &[0x40, 0x01], 0, 1, &[], 0x0141, 4),
            (4, &[0xFF, 0x01], 0, 1, &[], 0x0200, 5),
            (
                5,
                &[0xFE],
                2,
                0,
                &[(0x0000, 0x40), (0x0001, 0x01)],
                0x0140,
                6,
            ),
            (
                6,
                &[0xFF],
                0,
                1,
                &[(0x00FF, 0x40), (0x0000, 0x01)],
                0x0141,
                5,
            ),
            (
                6,
                &[0xFF],
                0,
                1,
                &[(0x00FF, 0xFF), (0x0000, 0x01)],
                0x0200,
                6,
            ),
        ];

        // 初始 A、内存值、按 AND/ORA/EOR 顺序排列的预期 A
        let values = [
            (0xA0, 0xC0, [0x80, 0xE0, 0x60]),
            (0x55, 0x55, [0x55, 0x55, 0x00]),
            (0x00, 0x80, [0x00, 0x80, 0x80]),
        ];

        for (operation, opcodes) in opcode_groups.iter().enumerate() {
            for &(mode, operands, x, y, pointers, target, cycles) in &scenarios {
                for &(initial_a, value, results) in &values {
                    let opcode = opcodes[mode];
                    let mut bus = Bus::new();
                    let mut cpu = Cpu::new();

                    cpu.pc = 0x0300;
                    cpu.a = initial_a;
                    cpu.x = x;
                    cpu.y = y;
                    cpu.status = FLAG_C | FLAG_V;

                    bus.cpu_write(0x0300, opcode).unwrap();
                    for (i, byte) in operands.iter().copied().enumerate() {
                        bus.cpu_write(0x0301 + i as u16, byte).unwrap();
                    }
                    for &(addr, byte) in pointers {
                        bus.cpu_write(addr, byte).unwrap();
                    }
                    bus.cpu_write(target, value).unwrap();

                    let expected_a = results[operation];
                    assert_eq!(cpu.step(&mut bus).unwrap(), cycles, "opcode {opcode:#04X}");
                    assert_eq!(cpu.pc, 0x0301 + operands.len() as u16);
                    assert_eq!(cpu.a, expected_a, "opcode {opcode:#04X}");
                    assert_eq!(bus.cpu_read(target).unwrap(), value);
                    assert_eq!(cpu.flag(FLAG_Z), expected_a == 0);
                    assert_eq!(cpu.flag(FLAG_N), expected_a & 0x80 != 0);
                    assert!(cpu.flag(FLAG_C), "opcode {opcode:#04X} changed C");
                    assert!(cpu.flag(FLAG_V), "opcode {opcode:#04X} changed V");
                }
            }
        }
    }

    #[test]
    fn indexed_memory_read_modify_write() {
        // 零页X操作码、绝对X操作码、旧值、输入C、
        // 新值、输出C、输出Z、输出N
        let cases = [
            (0x16, 0x1E, 0x80, false, 0x00, true, true, false), // ASL
            (0x56, 0x5E, 0x01, false, 0x00, true, true, false), // LSR
            (0x36, 0x3E, 0x80, true, 0x01, true, false, false), // ROL
            (0x76, 0x7E, 0x01, true, 0x80, true, false, true),  // ROR
            (0xF6, 0xFE, 0xFF, true, 0x00, true, true, false),  // INC 保留 C
            (0xD6, 0xDE, 0x00, false, 0xFF, false, false, true), // DEC 保留 C
        ];

        for (zp_opcode, abs_opcode, old, carry_in, new, carry_out, z, n) in cases {
            let scenarios: [(u8, &[u8], u8, u16, u8); 3] = [
                (zp_opcode, &[0xF0], 0x20, 0x0010, 6),        // 零页回绕
                (abs_opcode, &[0x40, 0x01], 0x01, 0x0141, 7), // 不跨页
                (abs_opcode, &[0xFF, 0x01], 0x01, 0x0200, 7), // 跨页也固定 7
            ];

            for (opcode, operands, x, target, cycles) in scenarios {
                let mut bus = Bus::new();
                let mut cpu = Cpu::new();
                cpu.pc = 0x0300;
                cpu.a = 0x5A; // 内存版本不应修改 A
                cpu.x = x;
                cpu.status = FLAG_V | if carry_in { FLAG_C } else { 0 };

                bus.cpu_write(0x0300, opcode).unwrap();
                for (i, byte) in operands.iter().copied().enumerate() {
                    bus.cpu_write(0x0301 + i as u16, byte).unwrap();
                }
                bus.cpu_write(target, old).unwrap();

                assert_eq!(cpu.step(&mut bus).unwrap(), cycles, "opcode {opcode:#04X}");
                assert_eq!(cpu.pc, 0x0301 + operands.len() as u16);
                assert_eq!(cpu.a, 0x5A);
                assert_eq!(bus.cpu_read(target).unwrap(), new, "opcode {opcode:#04X}");
                assert_eq!(cpu.flag(FLAG_C), carry_out);
                assert_eq!(cpu.flag(FLAG_Z), z);
                assert_eq!(cpu.flag(FLAG_N), n);
                assert!(cpu.flag(FLAG_V));
            }
        }
    }

    #[test]
    fn remaining_conditional_branches() {
        // 操作码、检查的标志、该标志为 1 时是否跳转
        let branches = [
            (0x90, FLAG_C, false), // BCC
            (0xB0, FLAG_C, true),  // BCS
            (0x10, FLAG_N, false), // BPL
            (0x30, FLAG_N, true),  // BMI
            (0x50, FLAG_V, false), // BVC
            (0x70, FLAG_V, true),  // BVS
        ];

        // 起始 PC、相对偏移、跳转后的 PC、跳转时的周期
        let positions = [
            (0x0300u16, 0x02u8, 0x0304u16, 3u8), // 正向，不跨页
            (0x02FD, 0x01, 0x0300, 4),           // 正向，跨页
            (0x0300, 0xFE, 0x0300, 3),           // -2，向后跳
        ];

        for (opcode, mask, take_when_set) in branches {
            for flag_set in [false, true] {
                for (start, offset, target, taken_cycles) in positions {
                    let mut bus = Bus::new();
                    let mut cpu = Cpu::new();
                    cpu.pc = start;
                    cpu.set_flag(mask, flag_set);

                    bus.cpu_write(start, opcode).unwrap();
                    bus.cpu_write(start + 1, offset).unwrap();

                    let taken = flag_set == take_when_set;
                    assert_eq!(
                        cpu.step(&mut bus).unwrap(),
                        if taken { taken_cycles } else { 2 },
                        "opcode {opcode:#04X}, flag_set={flag_set}"
                    );
                    assert_eq!(cpu.pc, if taken { target } else { start + 2 });
                    assert_eq!(cpu.flag(mask), flag_set);
                }
            }
        }
    }

    #[test]
    fn remaining_status_flag_instructions() {
        // 操作码、目标位、执行后是否置 1
        let cases = [
            (0xD8, FLAG_D, false), // CLD
            (0x58, FLAG_I, false), // CLI
            (0xB8, FLAG_V, false), // CLV
            (0xF8, FLAG_D, true),  // SED
            (0x78, FLAG_I, true),  // SEI
        ];

        for (opcode, mask, set) in cases {
            let mut bus = Bus::new();
            let mut cpu = Cpu::new();
            cpu.pc = 0x0300;

            // 同时放入其他标志，检查指令没有误改它们。
            cpu.status = FLAG_C | FLAG_Z | FLAG_N | FLAG_I | FLAG_D | FLAG_V;
            if set {
                cpu.status &= !mask;
            }
            let before = cpu.status;

            bus.cpu_write(0x0300, opcode).unwrap();
            assert_eq!(cpu.step(&mut bus).unwrap(), 2);
            assert_eq!(cpu.pc, 0x0301);
            assert_eq!(
                cpu.status,
                if set { before | mask } else { before & !mask },
                "opcode {opcode:#04X}"
            );
        }
    }

    #[test]
    fn sed_does_not_enable_decimal_arithmetic_on_nes() {
        let mut bus = Bus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x0300;
        cpu.a = 0x09;
        cpu.set_flag(FLAG_C, false);

        bus.cpu_write(0x0300, 0xF8).unwrap(); // SED
        bus.cpu_write(0x0301, 0x69).unwrap(); // ADC #$01
        bus.cpu_write(0x0302, 0x01).unwrap();

        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert!(cpu.flag(FLAG_D));
        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.a, 0x0A); // 二进制结果，不是 BCD 的 $10
    }

    #[test]
    fn php_pushes_status_with_bits_4_and_5_set() {
        for status in [0x00u8, 0x45, 0xCF] {
            for initial_sp in [0xFDu8, 0x00] {
                let mut bus = Bus::new();
                let mut cpu = Cpu::new();
                cpu.pc = 0x0300;
                cpu.sp = initial_sp;
                cpu.status = status;

                bus.cpu_write(0x0300, 0x08).unwrap();

                assert_eq!(cpu.step(&mut bus).unwrap(), 3);
                assert_eq!(cpu.pc, 0x0301);
                assert_eq!(cpu.sp, initial_sp.wrapping_sub(1));
                assert_eq!(cpu.status, status);
                assert_eq!(
                    bus.cpu_read(0x0100 | u16::from(initial_sp)).unwrap(),
                    status | 0x30
                );
            }
        }
    }

    #[test]
    fn plp_restores_flags_and_php_serializes_them_again() {
        for stacked in [0x00u8, 0x30, 0xCF, 0xFF, 0x82] {
            for initial_sp in [0xFCu8, 0xFF] {
                let mut bus = Bus::new();
                let mut cpu = Cpu::new();
                cpu.pc = 0x0300;
                cpu.sp = initial_sp;

                // 六个真实标志先设成与栈内容相反，验证设置和清除。
                cpu.status = (!stacked & 0xCF) | 0x20;

                bus.cpu_write(0x0300, 0x28).unwrap(); // PLP
                bus.cpu_write(0x0301, 0x08).unwrap(); // PHP

                let popped_sp = initial_sp.wrapping_add(1);
                let stack_addr = 0x0100 | u16::from(popped_sp);
                bus.cpu_write(stack_addr, stacked).unwrap();

                assert_eq!(cpu.step(&mut bus).unwrap(), 4);
                assert_eq!(cpu.pc, 0x0301);
                assert_eq!(cpu.sp, popped_sp);
                assert_eq!(cpu.status & 0xCF, stacked & 0xCF);

                // 无论原栈字节的第 4/5 位是什么，PHP 都应压入 11。
                assert_eq!(cpu.step(&mut bus).unwrap(), 3);
                assert_eq!(cpu.pc, 0x0302);
                assert_eq!(cpu.sp, initial_sp);
                assert_eq!(bus.cpu_read(stack_addr).unwrap(), (stacked & 0xCF) | 0x30);
            }
        }
    }

    #[test]
    fn indirect_jmp_handles_normal_and_page_boundary_pointers() {
        for (pointer, high_addr) in [(0x0240u16, 0x0241u16), (0x02FF, 0x0200)] {
            let mut bus = Bus::new();
            let mut cpu = Cpu::new();
            cpu.pc = 0x0400;
            cpu.sp = 0xFD;
            let status_before = cpu.status;

            let [low, high] = pointer.to_le_bytes();
            bus.cpu_write(0x0400, 0x6C).unwrap();
            bus.cpu_write(0x0401, low).unwrap();
            bus.cpu_write(0x0402, high).unwrap();

            bus.cpu_write(pointer, 0x56).unwrap();
            bus.cpu_write(high_addr, 0x04).unwrap();
            bus.cpu_write(0x0300, 0x07).unwrap(); // 抓住错误的跨页读取

            assert_eq!(cpu.step(&mut bus).unwrap(), 5);
            assert_eq!(cpu.pc, 0x0456);
            assert_eq!(cpu.sp, 0xFD);
            assert_eq!(cpu.status, status_before);
        }
    }

    // 放在 cpu.rs 的测试模块中。
    fn bus_with_brk_vector(handler: u16) -> Bus {
        const PRG_SIZE: usize = 16 * 1024;
        const CHR_SIZE: usize = 8 * 1024;

        let mut rom = vec![0u8; 16 + PRG_SIZE + CHR_SIZE];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1; // 16 KiB PRG ROM
        rom[5] = 1; // 8 KiB CHR ROM

        // 16 KiB Mapper 0 镜像下，$FFFE 对应 PRG 偏移 $3FFE。
        let vector_offset = 16 + 0x3FFE;
        rom[vector_offset..vector_offset + 2].copy_from_slice(&handler.to_le_bytes());

        let cartridge = crate::cartridge::Cartridge::try_from(rom.as_slice()).unwrap();

        let mut bus = Bus::new();
        bus.insert_cartridge(cartridge);
        bus
    }

    #[test]
    fn brk_and_rti_preserve_return_address_and_status() {
        // 同时验证：I=1 不能屏蔽 BRK。
        for initial_i in [false, true] {
            let mut bus = bus_with_brk_vector(0x0400);
            let mut cpu = Cpu::new();

            cpu.pc = 0x0300;
            cpu.sp = 0xFD;
            cpu.a = 0x5A;
            cpu.x = 0x12;
            cpu.y = 0x34;
            cpu.status = FLAG_C | FLAG_D | FLAG_V | FLAG_N;
            cpu.set_flag(FLAG_I, initial_i);
            let original_status = cpu.status;

            bus.cpu_write(0x0300, 0x00).unwrap(); // BRK
            bus.cpu_write(0x0301, 0xEA).unwrap(); // 被跳过的字节
            bus.cpu_write(0x0302, 0xEA).unwrap(); // 返回后执行的 NOP
            bus.cpu_write(0x0400, 0x40).unwrap(); // RTI

            assert_eq!(cpu.step(&mut bus).unwrap(), 7);
            assert_eq!(cpu.pc, 0x0400);
            assert_eq!(cpu.sp, 0xFA);

            // 单独检查栈内容，防止 BRK 和 RTI 的错误互相抵消。
            assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x03);
            assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x02);
            assert_eq!(bus.cpu_read(0x01FB).unwrap(), original_status | 0x30);
            assert_eq!(cpu.status & 0xCF, (original_status | FLAG_I) & 0xCF);

            // 模拟处理程序修改标志，RTI 必须从栈恢复。
            cpu.status = FLAG_I | FLAG_Z;

            assert_eq!(cpu.step(&mut bus).unwrap(), 6);
            assert_eq!(cpu.pc, 0x0302);
            assert_eq!(cpu.sp, 0xFD);
            assert_eq!(cpu.status & 0xCF, original_status & 0xCF);
            assert_eq!((cpu.a, cpu.x, cpu.y), (0x5A, 0x12, 0x34));

            assert_eq!(cpu.step(&mut bus).unwrap(), 2); // NOP
            assert_eq!(cpu.pc, 0x0303);
        }
    }

    #[test]
    fn rti_wraps_stack_and_restores_exact_pc() {
        let mut bus = Bus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x0400;
        cpu.sp = 0xFF;
        cpu.status = FLAG_I | FLAG_D;

        bus.cpu_write(0x0400, 0x40).unwrap(); // RTI

        // SP 从 $FF 回绕到 $00，再依次弹出三个字节。
        bus.cpu_write(0x0100, 0xF3).unwrap(); // 状态，包含第 4/5 位
        bus.cpu_write(0x0101, 0xFF).unwrap(); // PC 低字节
        bus.cpu_write(0x0102, 0x02).unwrap(); // PC 高字节

        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.sp, 0x02);
        assert_eq!(cpu.pc, 0x02FF); // 不得像 RTS 那样再加 1
        assert_eq!(cpu.status & 0xCF, 0xC3);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct CpuTrace {
        pc: u16,
        a: u8,
        x: u8,
        y: u8,
        p: u8,
        sp: u8,
        cycles: u64,
    }

    fn parse_trace(line: &str) -> CpuTrace {
        let field = |prefix: &str| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix(prefix))
                .unwrap_or_else(|| panic!("Missing {prefix} in: {line}"))
        };

        let hex_byte = |prefix: &str| {
            u8::from_str_radix(field(prefix), 16)
                .unwrap_or_else(|error| panic!("Invalid {prefix}: {error}"))
        };

        CpuTrace {
            pc: u16::from_str_radix(
                line.split_whitespace().next().expect("Empty trace line"),
                16,
            )
            .expect("Invalid PC"),
            a: hex_byte("A:"),
            x: hex_byte("X:"),
            y: hex_byte("Y:"),
            p: hex_byte("P:") & 0xCF,
            sp: hex_byte("SP:"),
            cycles: field("CYC:").parse().expect("Invalid cycle count"),
        }
    }

    #[test]
    fn nestest_official_prefix() {
        let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures");

        let rom = std::fs::read(fixture_dir.join("nestest.nes"))
            .expect("Cannot read tests/fixtures/nestest.nes");

        let reference = std::fs::read_to_string(fixture_dir.join("nestest.log"))
            .expect("Cannot read tests/fixtures/nestest.log");

        let cartridge = Cartridge::try_from(rom.as_slice()).expect("Cannot parse nestest ROM");

        let mut bus = Bus::new();
        bus.insert_cartridge(cartridge);

        let mut cpu = Cpu::new();
        cpu.pc = 0xC000;
        cpu.a = 0;
        cpu.x = 0;
        cpu.y = 0;
        cpu.status = 0x24;
        cpu.sp = 0xFD;

        let mut cycles = 7_u64;

        let lines: Vec<&str> = reference
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();

        // 执行 5003 条指令，再检查下一条执行前的状态。
        const INSTRUCTION_COUNT: usize = 5003;

        assert!(
            lines.len() > INSTRUCTION_COUNT,
            "Reference log is incomplete"
        );

        // 确认使用的是本测试针对的参考日志边界。
        let boundary = parse_trace(lines[INSTRUCTION_COUNT]);
        assert_eq!(boundary.pc, 0xC6BD);
        assert_eq!(boundary.cycles, 14579);
        assert_eq!(
            lines[INSTRUCTION_COUNT].split_whitespace().nth(1),
            Some("04")
        );

        for index in 0..=INSTRUCTION_COUNT {
            let expected = parse_trace(lines[index]);

            let actual = CpuTrace {
                pc: cpu.pc,
                a: cpu.a,
                x: cpu.x,
                y: cpu.y,
                p: cpu.status & 0xCF,
                sp: cpu.sp,
                cycles,
            };

            let previous = if index == 0 {
                "<initial state>"
            } else {
                lines[index - 1]
            };

            assert_eq!(
                actual,
                expected,
                "Mismatch at reference line {}\nPrevious: {}\nCurrent: {}",
                index + 1,
                previous,
                lines[index],
            );

            if index == INSTRUCTION_COUNT {
                break;
            }

            let elapsed = cpu.step(&mut bus).unwrap_or_else(|error| {
                panic!(
                    "Execution failed at reference line {}:\n{}\n{}",
                    index + 1,
                    lines[index],
                    error
                )
            });

            cycles += u64::from(elapsed);
        }
    }

    fn interrupt_test_bus() -> Bus {
        // iNES：1 个 16 KiB PRG，CHR RAM，Mapper 0。
        let mut rom = vec![0u8; 16 + 0x4000];
        rom[..4].copy_from_slice(b"NES\x1A");
        rom[4] = 1;

        let prg = &mut rom[16..];

        // NMI 处理程序 $9000：RTI
        prg[0x1000] = 0x40;

        // IRQ 处理程序 $A000：RTI
        prg[0x2000] = 0x40;

        // 16 KiB PRG 在 $C000..=$FFFF 镜像。
        prg[0x3FFA..0x3FFC].copy_from_slice(&0x9000u16.to_le_bytes());
        prg[0x3FFE..0x4000].copy_from_slice(&0xA000u16.to_le_bytes());

        let cartridge = crate::cartridge::Cartridge::try_from(rom.as_slice()).unwrap();

        let mut bus = Bus::new();
        bus.insert_cartridge(cartridge);
        bus
    }

    #[test]
    fn interrupt_nmi_preserves_old_status_and_returns() {
        // 同时验证：I=0 和 I=1 时，NMI 都可以进入。
        for old_status in [0x49u8, 0x4D] {
            let mut bus = interrupt_test_bus();
            let mut cpu = Cpu::new();
            cpu.pc = 0x8123;
            cpu.sp = 0xFD;
            cpu.status = old_status;
            cpu.a = 0x12;
            cpu.x = 0x34;
            cpu.y = 0x56;

            assert_eq!(cpu.nmi(&mut bus).unwrap(), 7);

            assert_eq!(cpu.pc, 0x9000);
            assert_eq!(cpu.sp, 0xFA);
            assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x81);
            assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x23);
            assert_eq!(bus.cpu_read(0x01FB).unwrap(), (old_status & !0x10) | 0x20);
            assert_eq!(cpu.status & 0xCF, old_status | FLAG_I);
            assert_eq!((cpu.a, cpu.x, cpu.y), (0x12, 0x34, 0x56));

            // 执行处理程序中的 RTI。
            assert_eq!(cpu.step(&mut bus).unwrap(), 6);
            assert_eq!(cpu.pc, 0x8123);
            assert_eq!(cpu.sp, 0xFD);
            assert_eq!(cpu.status & 0xCF, old_status);
            assert_eq!((cpu.a, cpu.x, cpu.y), (0x12, 0x34, 0x56));
        }
    }

    #[test]
    fn interrupt_irq_uses_irq_vector_and_returns() {
        let mut bus = interrupt_test_bus();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8456;
        cpu.sp = 0xFD;
        cpu.status = 0xC9; // N、V、D、C；I=0。

        assert_eq!(cpu.irq(&mut bus).unwrap(), 7);

        assert_eq!(cpu.pc, 0xA000);
        assert_eq!(cpu.sp, 0xFA);
        assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x84);
        assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x56);
        assert_eq!(bus.cpu_read(0x01FB).unwrap(), 0xE9);
        assert_eq!(cpu.status & 0xCF, 0xCD);

        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x8456);
        assert_eq!(cpu.sp, 0xFD);
        assert_eq!(cpu.status & 0xCF, 0xC9);
    }

    #[test]
    fn interrupt_masked_irq_changes_nothing() {
        // 故意不插卡带：被屏蔽时不应该读取中断向量。
        let mut bus = Bus::new();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8123;
        cpu.sp = 0xFD;
        cpu.status = 0xCD; // I=1。
        cpu.a = 0x12;
        cpu.x = 0x34;
        cpu.y = 0x56;

        for addr in 0x0100u16..=0x01FF {
            bus.cpu_write(addr, 0xA5).unwrap();
        }

        assert_eq!(cpu.irq(&mut bus).unwrap(), 0);

        assert_eq!(cpu.pc, 0x8123);
        assert_eq!(cpu.sp, 0xFD);
        assert_eq!(cpu.status, 0xCD);
        assert_eq!((cpu.a, cpu.x, cpu.y), (0x12, 0x34, 0x56));

        for addr in 0x0100u16..=0x01FF {
            assert_eq!(bus.cpu_read(addr).unwrap(), 0xA5);
        }
    }

    #[test]
    fn interrupt_stack_wraps_within_stack_page() {
        let mut bus = interrupt_test_bus();
        let mut cpu = Cpu::new();
        cpu.pc = 0x8123;
        cpu.sp = 0x00;
        cpu.status = 0x01;

        assert_eq!(cpu.nmi(&mut bus).unwrap(), 7);

        assert_eq!(cpu.sp, 0xFD);
        assert_eq!(bus.cpu_read(0x0100).unwrap(), 0x81);
        assert_eq!(bus.cpu_read(0x01FF).unwrap(), 0x23);
        assert_eq!(bus.cpu_read(0x01FE).unwrap(), 0x21);

        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x8123);
        assert_eq!(cpu.sp, 0x00);
        assert_eq!(cpu.status & 0xCF, 0x01);
    }

    fn signal_test_cpu() -> Cpu {
        let mut cpu = Cpu::new();
        cpu.pc = 0x0200;
        cpu.sp = 0xFD;
        cpu.status = 0;
        cpu
    }

    #[test]
    fn interrupt_signal_nmi_pulse_is_latched() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        cpu.status = FLAG_I; // NMI 不受 I 屏蔽。

        bus.cpu_write(0x0200, 0xEA).unwrap(); // NOP

        // 请求先产生，再撤销；事件仍应被记住。
        cpu.set_nmi_line(true);
        cpu.set_nmi_line(false);

        // setter 不应该立即进入中断。
        assert_eq!(cpu.pc, 0x0200);
        assert_eq!(cpu.sp, 0xFD);

        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(cpu.sp, 0xFA);

        // 中断入口没有执行原位置的 NOP。
        assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x02);
        assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x00);

        assert_eq!(cpu.step(&mut bus).unwrap(), 6); // RTI
        assert_eq!(cpu.pc, 0x0200);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // NOP
        assert_eq!(cpu.pc, 0x0201);
    }

    #[test]
    fn interrupt_signal_nmi_needs_a_new_edge() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        bus.cpu_write(0x0200, 0xEA).unwrap();

        cpu.set_nmi_line(true);
        assert_eq!(cpu.step(&mut bus).unwrap(), 7);

        // 持续有效，不能再次产生 NMI。
        cpu.set_nmi_line(true);
        assert_eq!(cpu.step(&mut bus).unwrap(), 6); // RTI

        cpu.set_nmi_line(true);
        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // NOP
        assert_eq!(cpu.pc, 0x0201);

        // 撤销后重新请求，才产生新事件。
        cpu.set_nmi_line(false);
        cpu.set_nmi_line(true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0x9000);
        assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x01);
    }

    #[test]
    fn interrupt_signal_irq_remains_asserted_after_entry() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        bus.cpu_write(0x0200, 0xEA).unwrap();

        cpu.set_irq_line(true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0xA000);

        // I=1，先执行处理程序中的 RTI。
        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x0200);
        assert!(!cpu.flag(FLAG_I));

        // IRQ 来源未撤销，RTI 后再次进入。
        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0xA000);

        cpu.set_irq_line(false);
        assert_eq!(cpu.step(&mut bus).unwrap(), 6); // RTI
        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // NOP
        assert_eq!(cpu.pc, 0x0201);
    }

    #[test]
    fn interrupt_signal_masked_irq_can_be_withdrawn() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        cpu.status = FLAG_I;

        bus.cpu_write(0x0200, 0xEA).unwrap();
        bus.cpu_write(0x0201, 0xEA).unwrap();

        cpu.set_irq_line(true);

        // 被屏蔽时，正常执行指令。
        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.pc, 0x0201);
        assert_eq!(cpu.sp, 0xFD);

        cpu.set_irq_line(false);

        // 直接设置测试初态，不是在测试 CLI 的采样延迟。
        cpu.set_flag(FLAG_I, false);

        // 已撤销的 IRQ 不应该像 NMI 一样被锁存。
        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.pc, 0x0202);
        assert_eq!(cpu.sp, 0xFD);
    }

    #[test]
    fn interrupt_signal_nmi_has_priority_without_clearing_irq() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();

        cpu.set_irq_line(true);
        cpu.set_nmi_line(true);

        // 两者同时请求，先进入 NMI。
        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0x9000);

        // NMI 处理程序返回，恢复 I=0。
        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x0200);
        assert_eq!(cpu.sp, 0xFD);

        // 持续有效的 NMI 不重复；IRQ 请求仍然有效。
        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0xA000);
    }

    #[test]
    fn interrupt_signal_pending_nmi_edges_coalesce() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        bus.cpu_write(0x0200, 0xEA).unwrap();

        // 处理之前发生多个边沿，只记录一个 pending。
        for _ in 0..3 {
            cpu.set_nmi_line(true);
            cpu.set_nmi_line(false);
        }

        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.step(&mut bus).unwrap(), 6); // RTI
        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // NOP
        assert_eq!(cpu.pc, 0x0201);
    }

    #[test]
    fn irq_timing_cli_allows_one_more_instruction() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        cpu.status = FLAG_I;

        // CLI; NOP; NOP
        write_irq_test_program(&mut bus, &[0x58, 0xEA, 0xEA]);
        cpu.set_irq_line(true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // CLI
        assert!(!cpu.flag(FLAG_I));
        assert_eq!(cpu.pc, 0x0201);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // 仍执行 NOP
        assert_eq!(cpu.pc, 0x0202);

        assert_eq!(cpu.step(&mut bus).unwrap(), 7); // IRQ
        assert_eq!(cpu.pc, 0xA000);
        assert_eq!(bus.cpu_read(0x01FD).unwrap(), 0x02);
        assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x02);
    }

    #[test]
    fn irq_timing_sei_does_not_cancel_irq_allowed_by_old_i() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        cpu.status = FLAG_I;

        // CLI; SEI; NOP
        write_irq_test_program(&mut bus, &[0x58, 0x78, 0xEA]);
        cpu.set_irq_line(true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // CLI
        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // SEI
        assert!(cpu.flag(FLAG_I));

        // 使用 SEI 执行前的 I=0，仍然进入 IRQ。
        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0xA000);

        // 采样用的是旧 I，但压栈必须保存当前状态的 I=1。
        assert_ne!(bus.cpu_read(0x01FB).unwrap() & FLAG_I, 0);

        cpu.set_irq_line(false);
        assert_eq!(cpu.step(&mut bus).unwrap(), 6); // RTI
        assert_eq!(cpu.pc, 0x0202);
        assert!(cpu.flag(FLAG_I));
    }

    #[test]
    fn irq_timing_plp_clearing_i_delays_irq() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        cpu.status = FLAG_I;

        // PLP; NOP
        write_irq_test_program(&mut bus, &[0x28, 0xEA]);

        // PLP 将从 $01FD 弹出 I=0。
        cpu.sp = 0xFC;
        bus.cpu_write(0x01FD, 0x20).unwrap();
        cpu.set_irq_line(true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 4); // PLP
        assert!(!cpu.flag(FLAG_I));
        assert_eq!(cpu.sp, 0xFD);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // NOP
        assert_eq!(cpu.pc, 0x0202);

        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0xA000);
    }

    #[test]
    fn irq_timing_plp_setting_i_uses_old_i_for_poll() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        cpu.status = FLAG_I;

        // CLI; PLP; NOP
        write_irq_test_program(&mut bus, &[0x58, 0x28, 0xEA]);

        cpu.sp = 0xFC;
        bus.cpu_write(0x01FD, 0x24).unwrap(); // 弹出 I=1
        cpu.set_irq_line(true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 2); // CLI
        assert_eq!(cpu.step(&mut bus).unwrap(), 4); // PLP
        assert!(cpu.flag(FLAG_I));

        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0xA000);
        assert_ne!(bus.cpu_read(0x01FB).unwrap() & FLAG_I, 0);
    }

    #[test]
    fn irq_timing_rti_uses_restored_i_without_delay() {
        let mut bus = interrupt_test_bus();
        let mut cpu = signal_test_cpu();
        cpu.status = FLAG_I;

        write_irq_test_program(&mut bus, &[0x40]); // RTI

        // RTI 依次弹出 P、PC low、PC high。
        cpu.sp = 0xFA;
        bus.cpu_write(0x01FB, 0x20).unwrap(); // I=0
        bus.cpu_write(0x01FC, 0x34).unwrap();
        bus.cpu_write(0x01FD, 0x02).unwrap();

        cpu.set_irq_line(true);

        assert_eq!(cpu.step(&mut bus).unwrap(), 6);
        assert_eq!(cpu.pc, 0x0234);
        assert!(!cpu.flag(FLAG_I));

        // 不执行 $0234 处的指令，直接进入 IRQ。
        assert_eq!(cpu.step(&mut bus).unwrap(), 7);
        assert_eq!(cpu.pc, 0xA000);
        assert_eq!(bus.cpu_read(0x01FC).unwrap(), 0x34);
    }

    #[test]
    fn interrupt_entry_executes_one_instruction_before_pending_nmi() {
        for entry in ["NMI", "IRQ", "BRK"] {
            let mut bus = interrupt_test_bus();
            let mut cpu = signal_test_cpu();
            cpu.status = 0; // I=0

            // BRK 和填充字节；其他两种入口不会执行它。
            bus.cpu_write(0x0200, 0x00).unwrap();
            bus.cpu_write(0x0201, 0xEA).unwrap();

            match entry {
                "NMI" => cpu.set_nmi_line(true),
                "IRQ" => cpu.set_irq_line(true),
                "BRK" => {}
                _ => unreachable!(),
            }

            // 第一次：进入中断。
            assert_eq!(cpu.step(&mut bus).unwrap(), 7, "{entry}");
            assert_eq!(
                cpu.pc,
                if entry == "NMI" { 0x9000 } else { 0xA000 },
                "{entry}"
            );
            assert_eq!(cpu.sp, 0xFA, "{entry}");

            // 撤销 IRQ，制造一个新的 NMI 边沿。
            cpu.set_irq_line(false);
            cpu.set_nmi_line(false);
            cpu.set_nmi_line(true);

            // 第二次：必须先执行处理程序第一条 RTI。
            // 没有 skip_interrupt_poll 时，这里会错误地返回 7。
            assert_eq!(cpu.step(&mut bus).unwrap(), 6, "{entry}");

            let return_pc = if entry == "BRK" { 0x0202 } else { 0x0200 };
            assert_eq!(cpu.pc, return_pc, "{entry}");
            assert_eq!(cpu.sp, 0xFD, "{entry}");

            // 第三次：之前等待的 NMI 没有丢失，现在进入。
            assert_eq!(cpu.step(&mut bus).unwrap(), 7, "{entry}");
            assert_eq!(cpu.pc, 0x9000, "{entry}");
            assert_eq!(cpu.sp, 0xFA, "{entry}");
        }
    }

    #[test]
    fn rmw_memory_writes_old_then_new_for_all_modes() {
        // 名称、四种寻址的操作码、旧值、新值、输出 C。
        // 所有用例的输入 C 都是 1。
        let operations = [
            ("ASL", [0x06u8, 0x16, 0x0E, 0x1E], 0x81u8, 0x02u8, true),
            ("LSR", [0x46, 0x56, 0x4E, 0x5E], 0x02, 0x01, false),
            ("ROL", [0x26, 0x36, 0x2E, 0x3E], 0x80, 0x01, true),
            ("ROR", [0x66, 0x76, 0x6E, 0x7E], 0x02, 0x81, false),
            ("INC", [0xE6, 0xF6, 0xEE, 0xFE], 0xFF, 0x00, true),
            ("DEC", [0xC6, 0xD6, 0xCE, 0xDE], 0x00, 0xFF, true),
        ];

        // 操作数字节、操作数长度、X、最终地址、周期。
        // 索引用例特意覆盖零页回绕和绝对地址跨页。
        let modes = [
            ([0x80u8, 0x00], 1usize, 0u8, 0x0080u16, 5u8),
            ([0xFF, 0x00], 1, 1, 0x0000, 6),
            ([0x80, 0x03], 2, 0, 0x0380, 6),
            ([0xFF, 0x03], 2, 1, 0x0400, 7),
        ];

        for (name, opcodes, old, expected, expected_c) in operations {
            for (mode_index, (operand, len, x, addr, cycles)) in modes.into_iter().enumerate() {
                let opcode = opcodes[mode_index];
                let mut bus = Bus::new();
                let mut cpu = Cpu::new();

                cpu.pc = 0x0200;
                cpu.sp = 0xFD;
                cpu.a = 0x5A;
                cpu.x = x;
                cpu.y = 0xA5;
                cpu.status = FLAG_C | FLAG_V | FLAG_D | FLAG_I;

                bus.cpu_write(0x0200, opcode).unwrap();
                for (offset, &byte) in operand[..len].iter().enumerate() {
                    bus.cpu_write(0x0201 + offset as u16, byte).unwrap();
                }
                bus.cpu_write(addr, old).unwrap();

                // 排除布置测试数据时产生的写入。
                let _ = bus.take_write_trace();

                assert_eq!(
                    cpu.step(&mut bus).unwrap(),
                    cycles,
                    "{name}, opcode={opcode:02X}"
                );

                assert_eq!(
                    bus.take_write_trace(),
                    vec![(addr, old), (addr, expected)],
                    "{name}, opcode={opcode:02X}: write sequence"
                );

                assert_eq!(bus.cpu_read(addr).unwrap(), expected);
                assert_eq!(cpu.pc, 0x0201 + len as u16);
                assert_eq!(cpu.sp, 0xFD);
                assert_eq!((cpu.a, cpu.x, cpu.y), (0x5A, x, 0xA5));

                assert_eq!(cpu.flag(FLAG_C), expected_c);
                assert_eq!(cpu.flag(FLAG_Z), expected == 0);
                assert_eq!(cpu.flag(FLAG_N), expected & 0x80 != 0);

                // 这些指令不应改变 V、D、I。
                assert!(cpu.flag(FLAG_V));
                assert!(cpu.flag(FLAG_D));
                assert!(cpu.flag(FLAG_I));
            }
        }
    }
}

// Paste this module at the end of src/cpu.rs.
// These tests record accesses performed by the CPU implementation.
// They do not establish cycle accuracy for all 6502 instructions.
#[cfg(test)]
mod cpu_bus_tests {
    use super::{Cpu, FLAG_N, FLAG_Z};
    use crate::bus::Bus;
    use crate::cpu_bus::CpuBus;

    #[derive(Debug, PartialEq, Eq)]
    enum Access {
        Read(u16),
        Write(u16, u8),
    }

    struct TraceBus {
        memory: Vec<u8>,
        accesses: Vec<Access>,
        fail_read: Option<u16>,
        fail_write: Option<u16>,
    }

    impl TraceBus {
        fn with_program(program: &[u8]) -> Self {
            let mut memory = vec![0; 65536];
            memory[0x8000..0x8000 + program.len()].copy_from_slice(program);
            Self {
                memory,
                accesses: Vec::new(),
                fail_read: None,
                fail_write: None,
            }
        }
    }

    impl CpuBus for TraceBus {
        fn cpu_read(&mut self, addr: u16) -> Result<u8, String> {
            self.accesses.push(Access::Read(addr));
            if self.fail_read == Some(addr) {
                return Err(format!("test read failure at {addr:04X}"));
            }
            Ok(self.memory[usize::from(addr)])
        }

        fn cpu_write(&mut self, addr: u16, value: u8) -> Result<(), String> {
            self.accesses.push(Access::Write(addr, value));
            if self.fail_write == Some(addr) {
                return Err(format!("test write failure at {addr:04X}"));
            }
            self.memory[usize::from(addr)] = value;
            Ok(())
        }
    }

    fn cpu_at_start() -> Cpu {
        let mut cpu = Cpu::new();
        cpu.pc = 0x8000;
        cpu
    }

    #[test]
    fn lda_immediate_uses_supplied_bus() {
        let mut cpu = cpu_at_start();
        let mut bus = TraceBus::with_program(&[0xA9, 0x80]);
        assert_eq!(cpu.step(&mut bus).unwrap(), 2);
        assert_eq!(cpu.a, 0x80);
        assert_eq!(cpu.pc, 0x8002);
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_Z));
        assert_eq!(
            bus.accesses,
            vec![Access::Read(0x8000), Access::Read(0x8001)]
        );
    }

    #[test]
    fn lda_absolute_preserves_read_order() {
        let mut cpu = cpu_at_start();
        let mut bus = TraceBus::with_program(&[0xAD, 0x00, 0x02]);
        bus.memory[0x0200] = 0x40;
        assert_eq!(cpu.step(&mut bus).unwrap(), 4);
        assert_eq!(cpu.a, 0x40);
        assert_eq!(cpu.pc, 0x8003);
        assert_eq!(
            bus.accesses,
            vec![
                Access::Read(0x8000),
                Access::Read(0x8001),
                Access::Read(0x8002),
                Access::Read(0x0200),
            ]
        );
    }

    #[test]
    fn sta_absolute_preserves_flags_and_write_order() {
        let mut cpu = cpu_at_start();
        let mut bus = TraceBus::with_program(&[0x8D, 0x00, 0x02]);
        cpu.a = 0x80;
        let status_before = cpu.status;
        assert_eq!(cpu.step(&mut bus).unwrap(), 4);
        assert_eq!(cpu.pc, 0x8003);
        assert_eq!(cpu.status, status_before);
        assert_eq!(bus.memory[0x0200], 0x80);
        assert_eq!(
            bus.accesses,
            vec![
                Access::Read(0x8000),
                Access::Read(0x8001),
                Access::Read(0x8002),
                Access::Write(0x0200, 0x80),
            ]
        );
    }

    #[test]
    fn load_store_load_chain_keeps_result_and_cycles() {
        let mut cpu = cpu_at_start();
        let mut bus = TraceBus::with_program(&[0xA9, 0x80, 0x8D, 0x00, 0x02, 0xAD, 0x00, 0x02]);
        let mut cycles = 0u64;
        for _ in 0..3 {
            cycles += u64::from(cpu.step(&mut bus).unwrap());
        }
        assert_eq!(cycles, 10);
        assert_eq!(cpu.a, 0x80);
        assert_eq!(cpu.pc, 0x8008);
        assert_eq!(bus.memory[0x0200], 0x80);
        assert_eq!(
            bus.accesses,
            vec![
                Access::Read(0x8000),
                Access::Read(0x8001),
                Access::Read(0x8002),
                Access::Read(0x8003),
                Access::Read(0x8004),
                Access::Write(0x0200, 0x80),
                Access::Read(0x8005),
                Access::Read(0x8006),
                Access::Read(0x8007),
                Access::Read(0x0200),
            ]
        );
    }

    #[test]
    fn opcode_read_error_reaches_caller() {
        let mut cpu = cpu_at_start();
        let mut bus = TraceBus::with_program(&[0xA9, 0x80]);
        bus.fail_read = Some(0x8000);
        let error = cpu.step(&mut bus).unwrap_err();
        assert!(error.contains("test read failure at 8000"), "{error}");
        assert_eq!(bus.accesses, vec![Access::Read(0x8000)]);
    }

    #[test]
    fn operand_read_error_reaches_caller() {
        let mut cpu = cpu_at_start();
        let mut bus = TraceBus::with_program(&[0xAD, 0x00, 0x02]);
        cpu.a = 0x55;
        bus.fail_read = Some(0x0200);
        let error = cpu.step(&mut bus).unwrap_err();
        assert!(error.contains("test read failure at 0200"), "{error}");
        assert_eq!(cpu.a, 0x55);
    }

    #[test]
    fn data_write_error_reaches_caller() {
        let mut cpu = cpu_at_start();
        let mut bus = TraceBus::with_program(&[0x8D, 0x00, 0x02]);
        cpu.a = 0x80;
        bus.fail_write = Some(0x0200);
        let error = cpu.step(&mut bus).unwrap_err();
        assert!(error.contains("test write failure at 0200"), "{error}");
        assert_eq!(bus.memory[0x0200], 0);
    }

    #[test]
    fn real_bus_trait_forwarding_preserves_ram_mirroring() {
        let mut bus = Bus::new();
        CpuBus::cpu_write(&mut bus, 0x0002, 0xA5).unwrap();
        assert_eq!(CpuBus::cpu_read(&mut bus, 0x0802).unwrap(), 0xA5);
        CpuBus::cpu_write(&mut bus, 0x1002, 0x3C).unwrap();
        assert_eq!(CpuBus::cpu_read(&mut bus, 0x0002).unwrap(), 0x3C);
    }
}
