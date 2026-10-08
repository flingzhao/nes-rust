const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

const NOISE_PERIODS: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

pub(super) struct NoiseChannel {
    enabled: bool,
    control: u8,

    mode: bool,
    period_index: u8,
    timer: u16,
    shift_register: u16,

    length_counter: u8,

    envelope_start: bool,
    envelope_divider: u8,
    envelope_decay: u8,
}

impl NoiseChannel {
    pub(super) fn new() -> Self {
        Self {
            enabled: false,
            control: 0,
            mode: false,
            period_index: 0,
            timer: 0,
            shift_register: 1,
            length_counter: 0,
            envelope_start: false,
            envelope_divider: 0,
            envelope_decay: 0,
        }
    }

    pub(super) fn write_register(&mut self, register: u8, value: u8) {
        match register {
            0x0000 => {
                self.control = value;
            }
            0x0001 => {}
            0x0002 => {
                self.mode = value & 0x80 != 0;
                self.period_index = value & 0x0F;
            }
            0x0003 => {
                if self.enabled {
                    self.length_counter = LENGTH_TABLE[usize::from(value >> 3)];
                }

                self.envelope_start = true;
            }
            _ => unreachable!(),
        }
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;

        if !self.enabled {
            self.length_counter = 0;
        }
    }

    pub(super) fn clock_cpu_cycle(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
        } else {
            self.timer = NOISE_PERIODS[usize::from(self.period_index)] - 1;

            let tap = if self.mode { 6 } else { 1 };
            let feedback = (self.shift_register & 1) ^ ((self.shift_register >> tap) & 1);
            self.shift_register = (self.shift_register >> 1) | (feedback << 14);
        }
    }

    pub(super) fn clock_quarter_frame(&mut self) {
        let reload_value = self.control & 0x0F;
        if self.envelope_start {
            self.envelope_start = false;
            self.envelope_decay = 15;
            self.envelope_divider = reload_value;
        } else {
            if self.envelope_divider > 0 {
                self.envelope_divider -= 1;
            } else {
                self.envelope_divider = reload_value;
                if self.envelope_decay > 0 {
                    self.envelope_decay -= 1;
                } else if self.control & 0x20 != 0 {
                    self.envelope_decay = 15;
                }
            }
        }
    }

    pub(super) fn clock_half_frame(&mut self) {
        if self.control & 0x20 == 0 && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    pub(super) fn is_active(&self) -> bool {
        self.length_counter > 0
    }

    pub(super) fn output(&self) -> u8 {
        if self.length_counter == 0 {
            return 0;
        }

        if self.shift_register & 1 == 1 {
            return 0;
        }

        let volume = if self.control & 0x10 != 0 {
            self.control & 0x0F
        } else {
            self.envelope_decay
        };
        volume
    }
}

#[cfg(test)]
mod noise_channel_tests {
    use super::NoiseChannel;
    use crate::apu::Apu;

    fn configured(control: u8) -> NoiseChannel {
        let mut noise = NoiseChannel::new();
        noise.set_enabled(true);
        noise.write_register(0, control);
        noise.write_register(2, 0);
        noise.write_register(3, 0); // 长度表索引 0 -> 10。
        noise
    }

    #[test]
    fn startup_and_disabled_length_loading() {
        let mut noise = NoiseChannel::new();
        assert_eq!(noise.shift_register, 1);
        assert_eq!(noise.timer, 0);
        assert!(!noise.is_active());
        assert_eq!(noise.output(), 0);
        noise.write_register(3, 0);
        assert_eq!(noise.length_counter, 0);
        assert!(noise.envelope_start);
        noise.set_enabled(true);
        assert_eq!(noise.length_counter, 0);
        noise.write_register(3, 0x18); // 索引 3 -> 2。
        assert_eq!(noise.length_counter, 2);
    }

    #[test]
    fn register_writes_do_not_reset_noise_sequence_or_current_timer() {
        let mut noise = configured(0x19);
        noise.shift_register = 0x0041;
        noise.timer = 7;
        noise.envelope_start = false;
        noise.envelope_decay = 4;
        noise.envelope_divider = 2;
        noise.write_register(0, 0x35);
        assert_eq!(noise.control, 0x35);
        assert!(!noise.envelope_start);
        noise.write_register(2, 0x8F);
        assert!(noise.mode);
        assert_eq!(noise.period_index, 15);
        noise.write_register(1, 0xFF); // $400D 无作用。
        noise.write_register(3, 0x18);
        assert_eq!(noise.length_counter, 2);
        assert!(noise.envelope_start);
        assert_eq!(noise.envelope_decay, 4);
        assert_eq!(noise.envelope_divider, 2);
        assert_eq!(noise.shift_register, 0x0041);
        assert_eq!(noise.timer, 7);
        noise.write_register(2, 0x70); // bit 4..6 不影响模式及周期索引。
        assert!(!noise.mode);
        assert_eq!(noise.period_index, 0);
    }

    #[test]
    fn feedback_uses_old_bit_one_or_old_bit_six() {
        let mut noise = NoiseChannel::new();
        for (mode, expected) in [(false, 0x4020), (true, 0x0020)] {
            noise.mode = mode;
            noise.shift_register = 0x0041; // bit 0=1, bit 1=0, bit 6=1。
            noise.timer = 0;
            noise.clock_cpu_cycle();
            assert_eq!(noise.shift_register, expected, "mode={mode}");
        }
    }

    #[test]
    fn all_ntsc_periods_are_cpu_cycle_intervals_not_reload_values() {
        let periods: [u16; 16] = [
            4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
        ];
        for (index, period) in periods.into_iter().enumerate() {
            let mut noise = NoiseChannel::new();
            noise.write_register(2, index as u8);
            noise.timer = period - 1;
            for _ in 0..period - 1 {
                noise.clock_cpu_cycle();
                assert_eq!(noise.shift_register, 1, "index={index}");
            }
            noise.clock_cpu_cycle();
            assert_eq!(noise.shift_register, 0x4000, "index={index}");
            assert_eq!(noise.timer, period - 1, "index={index}");
        }
    }

    #[test]
    fn timer_keeps_updating_noise_state_while_disabled() {
        let mut noise = NoiseChannel::new();
        noise.clock_cpu_cycle();
        assert_eq!(noise.shift_register, 0x4000);
        assert_eq!(noise.output(), 0);
        for _ in 0..4 {
            noise.clock_cpu_cycle();
        }
        assert_eq!(noise.shift_register, 0x2000);
        assert_eq!(noise.output(), 0);
    }

    #[test]
    fn output_uses_lowest_bit_and_constant_volume_and_length() {
        let mut noise = configured(0x19); // 固定音量 9，不暂停长度。
        noise.shift_register = 1;
        assert_eq!(noise.output(), 0);
        noise.shift_register = 0x4000;
        assert_eq!(noise.output(), 9);
        noise.write_register(0, 0x15);
        assert_eq!(noise.output(), 5);
        noise.set_enabled(false);
        assert_eq!(noise.length_counter, 0);
        assert_eq!(noise.output(), 0);
        assert_eq!(noise.shift_register, 0x4000);
        noise.set_enabled(true);
        assert_eq!(noise.output(), 0);
        noise.write_register(3, 0);
        assert_eq!(noise.output(), 5);
    }

    #[test]
    fn half_frame_uses_bit_five_not_bit_seven() {
        let mut noise = configured(0x80);
        noise.clock_half_frame();
        assert_eq!(noise.length_counter, 9);
        noise.write_register(0, 0x20);
        noise.clock_half_frame();
        assert_eq!(noise.length_counter, 9);
        noise.write_register(0, 0);
        for _ in 0..12 {
            noise.clock_half_frame();
        }
        assert_eq!(noise.length_counter, 0);
        assert!(!noise.is_active());
    }

    #[test]
    fn envelope_restart_is_deferred_and_decay_uses_divider_plus_one() {
        let mut noise = configured(2); // 包络模式，分频重装值为 2。
        noise.shift_register = 0x4000;
        assert_eq!(noise.output(), 0);
        noise.clock_quarter_frame();
        assert!(!noise.envelope_start);
        assert_eq!(noise.envelope_decay, 15);
        assert_eq!(noise.envelope_divider, 2);
        assert_eq!(noise.output(), 15);
        for expected in [15, 15, 14] {
            noise.clock_quarter_frame();
            assert_eq!(noise.output(), expected);
        }
        noise.write_register(3, 0);
        assert_eq!(noise.output(), 14);
        noise.clock_quarter_frame();
        assert_eq!(noise.output(), 15);
    }

    #[test]
    fn envelope_loops_only_when_bit_five_is_set() {
        for control in [0x00, 0x20] {
            let mut noise = configured(control);
            noise.clock_quarter_frame();
            for expected in (0..15).rev() {
                noise.clock_quarter_frame();
                assert_eq!(noise.envelope_decay, expected);
            }
            noise.clock_quarter_frame();
            assert_eq!(noise.envelope_decay, if control == 0x20 { 15 } else { 0 });
        }
    }

    #[test]
    fn constant_volume_does_not_freeze_envelope_updates() {
        let mut noise = configured(0x10); // 固定音量 0；包络分频值仍为 0。
        noise.shift_register = 0x4000;
        noise.clock_quarter_frame(); // decay = 15。
        noise.clock_quarter_frame(); // decay = 14。
        assert_eq!(noise.envelope_decay, 14);
        assert_eq!(noise.output(), 0);
        noise.write_register(0, 0); // 改为包络模式，不重新启动包络。
        assert_eq!(noise.output(), 14);
    }

    #[test]
    fn apu_routes_registers_timer_status_and_manual_frame_events() {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 0x08);
        apu.write_register(0x400C, 0x19);
        apu.write_register(0x400E, 0);
        apu.write_register(0x400F, 0);
        assert_eq!(apu.read_status() & 0x08, 0x08);
        assert_eq!(apu.noise_output(), 0); // 初始 shift_register=1。
        apu.clock_cpu_cycle();
        assert_eq!(apu.noise_output(), 9);
        apu.clock_quarter_frame();
        assert_eq!(apu.noise.envelope_decay, 15);
        apu.clock_half_frame();
        assert_eq!(apu.noise.length_counter, 9);
        apu.write_register(0x4015, 0);
        assert_eq!(apu.read_status() & 0x08, 0);
        assert_eq!(apu.noise_output(), 0);
    }

    #[test]
    fn frame_counter_automatically_drives_noise_envelope_and_length() {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 0x08);
        apu.write_register(0x400C, 0); // 包络模式，分频重装值为 0。
        apu.write_register(0x400E, 0);
        apu.write_register(0x400F, 0);
        for _ in 0..7457 {
            apu.clock_cpu_cycle();
        }
        assert_eq!(apu.noise.envelope_decay, 15);
        assert_eq!(apu.noise.length_counter, 10);
        for _ in 7457..14913 {
            apu.clock_cpu_cycle();
        }
        assert_eq!(apu.noise.envelope_decay, 14);
        assert_eq!(apu.noise.length_counter, 9);
    }
}
