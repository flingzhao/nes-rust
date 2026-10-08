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
        }
    }

    pub(super) fn write_register(&mut self, register: u8, value: u8) {
        match register {
            0x0000 => {
                self.control = value;
            }
            0x0001 => {
                self.output_level = value & 0x7F;
            }
            0x0002 => {}
            0x0003 => {}
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
