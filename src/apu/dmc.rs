const DMC_PERIODS: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];

pub(super) struct DmcChannel {
    control: u8,
    timer: u16,

    output_level: u8,

    shift_register: u8,
    bits_remaining: u8,
    silence: bool,

    sample_buffer: Option<u8>,

    sample_address: u16,
    sample_length: u16,
    current_address: u16,
    bytes_remaining: u16,
    irq_pending: bool,
}

impl DmcChannel {
    pub(super) fn new() -> Self {
        Self {
            control: 0,
            timer: 0,
            output_level: 0,
            shift_register: 0,
            bits_remaining: 8,
            silence: true,
            sample_buffer: None,
            sample_address: 0xC000,
            sample_length: 1,
            current_address: 0xC000,
            bytes_remaining: 0,
            irq_pending: false,
        }
    }

    pub(super) fn write_register(&mut self, register: u8, value: u8) {
        match register {
            0x0000 => {
                self.control = value;

                if value & 0x80 == 0 {
                    self.irq_pending = false;
                }
            }
            0x0001 => {
                self.output_level = value & 0x7F;
            }
            0x0002 => {
                self.sample_address = 0xC000 + (u16::from(value) << 6);
            }
            0x0003 => {
                self.sample_length = (u16::from(value) << 4) + 1;
            }
            _ => unreachable!(),
        }
    }

    pub(super) fn fill_sample_buffer(&mut self, value: u8) -> Result<(), String> {
        match self.sample_buffer {
            Some(_) => return Err("sample buffer already has value".to_string()),
            None => self.sample_buffer = Some(value),
        };
        Ok(())
    }

    pub(super) fn clock_cpu_cycle(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
        } else {
            let rate_index = usize::from(self.control & 0x0F);
            self.timer = DMC_PERIODS[rate_index] - 1;
            self.clock_output_bit();
        }
    }

    pub(super) fn clock_output_bit(&mut self) {
        if !self.silence {
            if self.shift_register & 1 == 1 && self.output_level <= 125 {
                self.output_level += 2
            } else if self.shift_register & 1 == 0 && self.output_level >= 2 {
                self.output_level -= 2;
            }
        }

        self.shift_register >>= 1;
        self.bits_remaining = self.bits_remaining.saturating_sub(1);

        if self.bits_remaining == 0 {
            self.bits_remaining = 8;
            match self.sample_buffer.take() {
                Some(value) => {
                    self.shift_register = value;
                    self.silence = false;
                }
                None => {
                    self.silence = true;
                }
            }
        }
    }

    pub(super) fn output(&self) -> u8 {
        self.output_level
    }

    fn restart_sample(&mut self) {
        self.current_address = self.sample_address;
        self.bytes_remaining = self.sample_length;
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.irq_pending = false;

        if !enabled {
            self.bytes_remaining = 0;
        } else if self.bytes_remaining == 0 {
            self.restart_sample();
        }
    }

    pub(super) fn next_sample_address(&self) -> Option<u16> {
        if self.sample_buffer == None && self.bytes_remaining > 0 {
            Some(self.current_address)
        } else {
            None
        }
    }

    pub(super) fn complete_sample_read(&mut self, value: u8) -> Result<(), String> {
        if self.sample_buffer.is_some() {
            return Err("sample buffer still has value".to_string());
        }
        if self.bytes_remaining == 0 {
            return Err("there is no more byte remaining".to_string());
        }

        self.fill_sample_buffer(value)?;

        if self.current_address != 0xFFFF {
            self.current_address += 1;
        } else {
            self.current_address = 0x8000;
        }

        self.bytes_remaining -= 1;

        if self.bytes_remaining == 0 {
            if self.control & 0x40 != 0 {
                self.restart_sample();
            } else if self.control & 0x80 != 0 {
                self.irq_pending = true;
            }
        }

        Ok(())
    }

    pub(super) fn is_active(&self) -> bool {
        self.bytes_remaining > 0
    }

    pub(super) fn irq_pending(&self) -> bool {
        self.irq_pending
    }
}

#[cfg(test)]
mod dmc_output_tests {
    use super::DmcChannel;
    use crate::apu::Apu;

    fn playing(level: u8, byte: u8) -> DmcChannel {
        let mut dmc = DmcChannel::new();
        dmc.output_level = level;
        dmc.shift_register = byte;
        dmc.bits_remaining = 8;
        dmc.silence = false;
        dmc
    }

    #[test]
    fn initial_state_and_direct_load_mask() {
        let mut dmc = DmcChannel::new();
        assert_eq!(dmc.control, 0);
        assert_eq!(dmc.timer, 0);
        assert_eq!(dmc.output(), 0);
        assert_eq!(dmc.bits_remaining, 8);
        assert!(dmc.silence);
        assert_eq!(dmc.sample_buffer, None);
        for (value, expected) in [(0x40, 64), (0xFF, 127), (0x80, 0)] {
            dmc.write_register(1, value);
            assert_eq!(dmc.output(), expected);
        }
    }

    #[test]
    fn direct_load_does_not_restart_current_byte_or_timer() {
        let mut dmc = playing(64, 0xA5);
        dmc.bits_remaining = 5;
        dmc.timer = 19;
        dmc.fill_sample_buffer(0x12).unwrap();
        dmc.write_register(1, 0x20);
        assert_eq!(dmc.output(), 32);
        assert_eq!(dmc.shift_register, 0xA5);
        assert_eq!(dmc.bits_remaining, 5);
        assert_eq!(dmc.timer, 19);
        assert_eq!(dmc.sample_buffer, Some(0x12));
        assert!(!dmc.silence);
    }

    #[test]
    fn a5_is_consumed_least_significant_bit_first() {
        let mut dmc = playing(64, 0xA5);
        for expected in [66, 64, 66, 64, 62, 64, 62, 64] {
            dmc.clock_output_bit();
            assert_eq!(dmc.output(), expected);
        }
        assert_eq!(dmc.shift_register, 0);
        assert_eq!(dmc.bits_remaining, 8);
        assert!(dmc.silence);
    }

    #[test]
    fn out_of_range_delta_keeps_level_instead_of_clamping_or_wrapping() {
        for (initial, bit, expected) in [
            (124, 1, 126),
            (125, 1, 127),
            (126, 1, 126),
            (127, 1, 127),
            (3, 0, 1),
            (2, 0, 0),
            (1, 0, 1),
            (0, 0, 0),
        ] {
            let mut dmc = playing(initial, bit);
            dmc.clock_output_bit();
            assert_eq!(dmc.output(), expected, "level={initial}, bit={bit}");
        }
    }

    #[test]
    fn silence_holds_output_but_continues_shifting_and_counting() {
        let mut dmc = playing(43, 3);
        dmc.silence = true;
        dmc.bits_remaining = 2;
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 43);
        assert_eq!(dmc.shift_register, 1);
        assert_eq!(dmc.bits_remaining, 1);
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 43);
        assert_eq!(dmc.shift_register, 0);
        assert_eq!(dmc.bits_remaining, 8);
        assert!(dmc.silence);
    }

    #[test]
    fn fill_rejects_overwrite_without_discarding_original_byte() {
        let mut dmc = DmcChannel::new();
        assert!(dmc.fill_sample_buffer(0xA5).is_ok());
        assert!(dmc.fill_sample_buffer(0xFF).is_err());
        assert_eq!(dmc.sample_buffer, Some(0xA5));
    }

    #[test]
    fn initial_silent_group_finishes_before_loading_buffered_byte() {
        let mut dmc = DmcChannel::new();
        dmc.write_register(1, 64);
        dmc.fill_sample_buffer(0xA5).unwrap();
        assert!(dmc.silence);
        for remaining in (1..8).rev() {
            dmc.clock_output_bit();
            assert_eq!(dmc.bits_remaining, remaining);
            assert_eq!(dmc.output(), 64);
            assert_eq!(dmc.sample_buffer, Some(0xA5));
        }
        dmc.clock_output_bit();
        assert_eq!(dmc.bits_remaining, 8);
        assert_eq!(dmc.shift_register, 0xA5);
        assert_eq!(dmc.sample_buffer, None);
        assert!(!dmc.silence);
        assert_eq!(dmc.output(), 64); // 装载新字节时尚未处理新字节的 bit 0。
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 66);
    }

    #[test]
    fn last_old_bit_is_processed_before_loading_next_byte() {
        let mut dmc = playing(64, 1);
        dmc.bits_remaining = 1;
        dmc.fill_sample_buffer(0x00).unwrap();
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 66);
        assert_eq!(dmc.shift_register, 0);
        assert_eq!(dmc.sample_buffer, None);
        assert!(!dmc.silence); // Some(0) 仍然是一个有效的样本字节。
        assert_eq!(dmc.bits_remaining, 8);
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 64);
    }

    #[test]
    fn buffer_refill_during_silence_waits_for_current_group_end() {
        let mut dmc = DmcChannel::new();
        dmc.write_register(1, 64);
        dmc.bits_remaining = 2;
        dmc.fill_sample_buffer(1).unwrap();
        dmc.clock_output_bit();
        assert!(dmc.silence);
        assert_eq!(dmc.bits_remaining, 1);
        assert_eq!(dmc.output(), 64);
        dmc.clock_output_bit();
        assert!(!dmc.silence);
        assert_eq!(dmc.output(), 64);
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 66);
    }

    #[test]
    fn every_ntsc_rate_is_an_interval_in_cpu_cycles() {
        let periods: [u16; 16] = [
            428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
        ];
        for (index, period) in periods.into_iter().enumerate() {
            let mut dmc = playing(64, 0xFF);
            dmc.write_register(0, index as u8);
            dmc.timer = period - 1;
            for _ in 0..period - 1 {
                dmc.clock_cpu_cycle();
                assert_eq!(dmc.output(), 64, "index={index}");
                assert_eq!(dmc.bits_remaining, 8);
            }
            dmc.clock_cpu_cycle();
            assert_eq!(dmc.output(), 66, "index={index}");
            assert_eq!(dmc.bits_remaining, 7);
            assert_eq!(dmc.timer, period - 1);
        }
    }

    #[test]
    fn rate_write_preserves_current_timer_and_byte() {
        let mut dmc = playing(64, 0xA5);
        dmc.timer = 19;
        dmc.fill_sample_buffer(0x12).unwrap();
        dmc.write_register(0, 0xCF);
        assert_eq!(dmc.control, 0xCF);
        assert_eq!(dmc.timer, 19);
        assert_eq!(dmc.shift_register, 0xA5);
        assert_eq!(dmc.bits_remaining, 8);
        assert_eq!(dmc.sample_buffer, Some(0x12));
        assert_eq!(dmc.output(), 64);
        // 本轮暂存高位，不测试尚未实现的 IRQ 和循环读取行为。
    }

    #[test]
    fn apu_direct_load_works_without_dmc_enable_and_ignores_frame_events() {
        let mut apu = Apu::new();
        apu.write_register(0x4011, 0xC0);
        assert_eq!(apu.dmc_output(), 64);
        apu.write_register(0x4015, 0);
        assert_eq!(apu.dmc_output(), 64);
        apu.clock_quarter_frame();
        apu.clock_half_frame();
        assert_eq!(apu.dmc.bits_remaining, 8);
        assert_eq!(apu.dmc_output(), 64);
        apu.clock_cpu_cycle();
        assert_eq!(apu.dmc.bits_remaining, 7);
        assert_eq!(apu.dmc_output(), 64);
        assert_eq!(apu.read_status() & 0x10, 0); // 有电平不等于仍有待读取样本。
    }

    #[test]
    fn apu_clocks_output_unit_and_preserves_byte_boundary_order() {
        let mut apu = Apu::new();
        apu.write_register(0x4010, 0x0F); // 54 个 CPU 周期处理一位。
        apu.write_register(0x4011, 64);
        apu.dmc.fill_sample_buffer(0xA5).unwrap();
        // timer 初始为 0：第 1、55、109……379 周期完成首组 8 次处理。
        for _ in 0..379 {
            apu.clock_cpu_cycle();
        }
        assert_eq!(apu.dmc.sample_buffer, None);
        assert_eq!(apu.dmc.shift_register, 0xA5);
        assert_eq!(apu.dmc.bits_remaining, 8);
        assert_eq!(apu.dmc_output(), 64);
        for _ in 0..54 {
            apu.clock_cpu_cycle();
        }
        assert_eq!(apu.dmc_output(), 66);
        assert_eq!(apu.dmc.bits_remaining, 7);
    }
}

#[cfg(test)]
mod dmc_reader_tests {
    use super::*;

    fn configured(control: u8, address: u8, length: u8) -> DmcChannel {
        let mut dmc = DmcChannel::new();
        dmc.write_register(0, control);
        dmc.write_register(2, address);
        dmc.write_register(3, length);
        dmc
    }

    #[test]
    fn reader_defaults_and_register_limits() {
        let mut dmc = DmcChannel::new();
        assert_eq!(dmc.sample_address, 0xC000);
        assert_eq!(dmc.sample_length, 1);
        assert!(!dmc.is_active());
        assert!(!dmc.irq_pending());
        assert_eq!(dmc.next_sample_address(), None);
        dmc.write_register(2, 0xFF);
        dmc.write_register(3, 0xFF);
        assert_eq!(dmc.sample_address, 0xFFC0);
        assert_eq!(dmc.sample_length, 4081);
    }

    #[test]
    fn start_and_complete_a_read_without_changing_output() {
        let mut dmc = configured(0, 2, 1);
        dmc.write_register(1, 64);
        dmc.set_enabled(true);
        assert_eq!(dmc.current_address, 0xC080);
        assert_eq!(dmc.bytes_remaining, 17);
        assert_eq!(dmc.next_sample_address(), Some(0xC080));
        assert_eq!(dmc.next_sample_address(), Some(0xC080));
        dmc.complete_sample_read(0xA5).unwrap();
        assert_eq!(dmc.sample_buffer, Some(0xA5));
        assert_eq!(dmc.current_address, 0xC081);
        assert_eq!(dmc.bytes_remaining, 16);
        assert_eq!(dmc.next_sample_address(), None);
        assert_eq!(dmc.output(), 64);

        for _ in 0..8 {
            dmc.clock_output_bit();
        }
        assert_eq!(dmc.shift_register, 0xA5);
        assert_eq!(dmc.next_sample_address(), Some(0xC081));
        assert_eq!(dmc.output(), 64);
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 66);
    }

    #[test]
    fn invalid_completions_do_not_change_reader_state() {
        let mut dmc = configured(0, 2, 1);
        assert!(dmc.complete_sample_read(0x11).is_err());
        assert_eq!(dmc.bytes_remaining, 0);
        assert_eq!(dmc.sample_buffer, None);
        dmc.set_enabled(true);
        dmc.complete_sample_read(0xA5).unwrap();
        assert!(dmc.complete_sample_read(0xFF).is_err());
        assert_eq!(dmc.sample_buffer, Some(0xA5));
        assert_eq!(dmc.current_address, 0xC081);
        assert_eq!(dmc.bytes_remaining, 16);
    }

    #[test]
    fn configuration_writes_do_not_restart_an_active_reader() {
        let mut dmc = configured(0, 2, 1);
        dmc.set_enabled(true);
        dmc.complete_sample_read(0xA5).unwrap();
        dmc.write_register(2, 3);
        dmc.write_register(3, 2);
        dmc.set_enabled(true);
        assert_eq!(dmc.current_address, 0xC081);
        assert_eq!(dmc.bytes_remaining, 16);
        assert_eq!(dmc.sample_address, 0xC0C0);
        assert_eq!(dmc.sample_length, 33);
        dmc.set_enabled(false);
        dmc.set_enabled(true);
        assert_eq!(dmc.current_address, 0xC0C0);
        assert_eq!(dmc.bytes_remaining, 33);
        assert_eq!(dmc.sample_buffer, Some(0xA5));
        assert_eq!(dmc.next_sample_address(), None);
    }

    #[test]
    fn disable_preserves_buffer_shifter_and_output() {
        let mut dmc = configured(0, 0, 1);
        dmc.write_register(1, 64);
        dmc.set_enabled(true);
        dmc.complete_sample_read(0xA5).unwrap();
        dmc.shift_register = 0xFF;
        dmc.bits_remaining = 3;
        dmc.silence = false;
        dmc.timer = 9;
        dmc.set_enabled(false);
        assert!(!dmc.is_active());
        assert_eq!(dmc.next_sample_address(), None);
        assert_eq!(dmc.sample_buffer, Some(0xA5));
        assert_eq!(dmc.shift_register, 0xFF);
        assert_eq!(dmc.bits_remaining, 3);
        assert_eq!(dmc.timer, 9);
        assert!(!dmc.silence);
        assert_eq!(dmc.output(), 64);
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 66);
    }

    #[test]
    fn address_wraps_to_8000_not_0000() {
        let mut dmc = configured(0, 0xFF, 4); // 65 bytes from $FFC0
        dmc.set_enabled(true);
        for offset in 0..64u16 {
            assert_eq!(dmc.next_sample_address(), Some(0xFFC0 + offset));
            dmc.complete_sample_read(0).unwrap();
            // Test-only: stand in for the output unit consuming the buffer.
            dmc.sample_buffer.take();
        }
        assert_eq!(dmc.next_sample_address(), Some(0x8000));
        assert_eq!(dmc.bytes_remaining, 1);
        dmc.complete_sample_read(0xA5).unwrap();
        assert_eq!(dmc.current_address, 0x8001);
        assert!(!dmc.is_active());
        assert!(!dmc.irq_pending());
    }

    #[test]
    fn last_fetch_sets_irq_before_the_byte_is_played() {
        let mut dmc = configured(0x80, 2, 0); // IRQ, no loop, one byte
        dmc.write_register(1, 64);
        dmc.set_enabled(true);
        dmc.complete_sample_read(0xA5).unwrap();
        assert!(!dmc.is_active());
        assert!(dmc.irq_pending());
        assert_eq!(dmc.sample_buffer, Some(0xA5));
        assert_eq!(dmc.output(), 64);
        for _ in 0..8 {
            dmc.clock_output_bit();
        }
        dmc.clock_output_bit();
        assert_eq!(dmc.output(), 66);
        assert!(dmc.irq_pending());
    }

    #[test]
    fn loop_restarts_reader_without_discarding_the_last_byte() {
        let mut dmc = configured(0xC0, 2, 0); // IRQ + loop: loop wins
        dmc.set_enabled(true);
        dmc.complete_sample_read(0xA5).unwrap();
        assert_eq!(dmc.current_address, 0xC080);
        assert_eq!(dmc.bytes_remaining, 1);
        assert!(dmc.is_active());
        assert!(!dmc.irq_pending());
        assert_eq!(dmc.sample_buffer, Some(0xA5));
        assert_eq!(dmc.next_sample_address(), None);
        for _ in 0..8 {
            dmc.clock_output_bit();
        }
        assert_eq!(dmc.next_sample_address(), Some(0xC080));
    }

    #[test]
    fn irq_clearing_does_not_change_output() {
        let mut dmc = configured(0x80, 0, 0);
        dmc.write_register(1, 64);
        dmc.set_enabled(true);
        dmc.complete_sample_read(0xA5).unwrap();
        dmc.write_register(0, 0x8F);
        assert!(dmc.irq_pending());
        dmc.write_register(0, 0x0F);
        assert!(!dmc.irq_pending());
        assert_eq!(dmc.output(), 64);
        for enabled in [false, true] {
            dmc.irq_pending = true; // Isolate the $4015 acknowledgement rule.
            dmc.set_enabled(enabled);
            assert!(!dmc.irq_pending());
            assert_eq!(dmc.output(), 64);
        }
    }

    #[test]
    fn apu_status_and_irq_follow_reader_not_output_level() {
        use super::super::Apu;
        let mut apu = Apu::new();
        apu.write_register(0x4011, 64);
        assert_eq!(apu.read_status() & 0x90, 0);
        apu.write_register(0x4010, 0x80);
        apu.write_register(0x4012, 2);
        apu.write_register(0x4013, 0);
        apu.write_register(0x4015, 0x10);
        assert_eq!(apu.read_status() & 0x90, 0x10);
        apu.dmc.complete_sample_read(0xA5).unwrap();
        assert_eq!(apu.read_status() & 0x90, 0x80);
        assert_eq!(apu.read_status() & 0x90, 0x80); // Read must not clear DMC IRQ.
        assert!(apu.irq_line());
        assert_eq!(apu.dmc_output(), 64);
        apu.write_register(0x4015, 0);
        assert_eq!(apu.read_status() & 0x90, 0);
        assert!(!apu.irq_line());
        assert_eq!(apu.dmc_output(), 64);
    }
}
