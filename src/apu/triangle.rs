const TRIANGLE_SEQUENCE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

pub(super) struct TriangleChannel {
    enabled: bool,
    control: u8,
    timer_period: u16,
    timer: u16,
    sequence_step: u8,
    length_counter: u8,
    linear_counter: u8,
    linear_reload: bool,
}

impl TriangleChannel {
    pub(super) fn new() -> Self {
        Self {
            enabled: false,
            control: 0,
            timer_period: 0,
            timer: 0,
            sequence_step: 0,
            length_counter: 0,
            linear_counter: 0,
            linear_reload: false,
        }
    }

    pub(super) fn write_register(&mut self, register: u8, value: u8) {
        match register {
            0x0000 => {
                self.control = value;
            }
            0x0001 => {}
            0x0002 => {
                self.timer_period = (self.timer_period & 0xFF00) | u16::from(value);
            }
            0x0003 => {
                self.timer_period = (self.timer_period & 0x00FF) | u16::from(value & 0x07) << 8;

                if self.enabled {
                    let index = usize::from(value >> 3);
                    self.length_counter = LENGTH_TABLE[index];
                }

                self.linear_reload = true;
            }
            _ => unreachable!(),
        }
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;

        if !enabled {
            self.length_counter = 0;
        }
    }

    pub(super) fn clock_cpu_cycle(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
        } else {
            self.timer = self.timer_period;
            if self.length_counter > 0 && self.linear_counter > 0 {
                self.sequence_step = (self.sequence_step + 1) % 32;
            }
        }
    }

    pub(super) fn clock_quarter_frame(&mut self) {
        // 有重装请求：将 linear_counter 设为 R。
        // 没有请求：若计数器大于零，将它减一。
        // 最后检查 C：如果 C 为零，清除重装请求。
        // TODO: 计数器是哪个
        if self.linear_reload {
            self.linear_counter = self.control & 0x7F;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if self.control & 0x80 == 0 {
            self.linear_reload = false;
        }
    }

    pub(super) fn clock_half_frame(&mut self) {
        if self.control & 0x80 == 0 && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    pub(super) fn is_active(&self) -> bool {
        self.length_counter > 0
    }

    pub(super) fn output(&self) -> u8 {
        TRIANGLE_SEQUENCE[usize::from(self.sequence_step)]
    }
}

// 将整个模块追加到 src/apu/triangle.rs 末尾。
// 对应本轮约定：TriangleChannel 使用 control、timer_period、timer、
// sequence_step、length_counter、linear_counter、linear_reload 字段。
// Apu 新增 triangle 字段及 triangle_output()，其余接口保持不变。
// new() 的 sequence_step = 0、timer = 0 是测试用的软件初始约定，
// 不意味着真实机器上电时必然具有这个相位。

#[cfg(test)]
mod triangle_channel_tests {
    use super::TriangleChannel;
    use crate::apu::Apu;

    fn configured(control: u8, period: u16) -> TriangleChannel {
        let mut channel = TriangleChannel::new();
        channel.set_enabled(true);
        channel.write_register(0, control);
        channel.write_register(2, period as u8);
        // 长度表索引 0 -> 10。
        channel.write_register(3, (period >> 8) as u8 & 7);
        channel
    }

    #[test]
    fn disabled_channel_cannot_load_length_and_enabling_does_not_reload_it() {
        let mut channel = TriangleChannel::new();
        channel.write_register(3, 0);
        assert_eq!(channel.length_counter, 0);
        assert!(channel.linear_reload);
        channel.set_enabled(true);
        assert_eq!(channel.length_counter, 0);
        channel.write_register(3, 0);
        assert_eq!(channel.length_counter, 10);
        assert!(channel.is_active());
    }

    #[test]
    fn register_writes_preserve_timer_countdown_and_wave_phase() {
        let mut channel = configured(3, 0x123);
        channel.timer = 7;
        channel.sequence_step = 5;
        channel.linear_counter = 2;
        channel.linear_reload = false;

        channel.write_register(0, 0x85);
        assert_eq!(channel.control, 0x85);
        assert_eq!(channel.linear_counter, 2);
        assert!(!channel.linear_reload);

        channel.write_register(2, 0xAB);
        assert_eq!(channel.timer_period, 0x1AB);
        channel.write_register(3, 0xAF); // 索引 21 -> 20；高三位 = 7。
        assert_eq!(channel.timer_period, 0x7AB);
        assert_eq!(channel.length_counter, 20);
        assert!(channel.linear_reload);
        assert_eq!(channel.timer, 7);
        assert_eq!(channel.sequence_step, 5);
        assert_eq!(channel.linear_counter, 2);

        channel.write_register(1, 0xFF); // $4009 没有作用。
        assert_eq!(channel.timer_period, 0x7AB);
        assert_eq!(channel.control, 0x85);
        assert_eq!(channel.timer, 7);
        assert_eq!(channel.sequence_step, 5);
    }

    #[test]
    fn quarter_frame_reloads_then_counts_down_without_underflow() {
        let mut channel = configured(3, 126);
        assert_eq!(channel.linear_counter, 0);
        for expected in [3, 2, 1, 0, 0] {
            channel.clock_quarter_frame();
            assert_eq!(channel.linear_counter, expected);
            assert!(!channel.linear_reload);
        }
    }

    #[test]
    fn control_flag_keeps_reload_request_until_cleared() {
        let mut channel = configured(0x83, 126);
        for _ in 0..3 {
            channel.clock_quarter_frame();
            assert_eq!(channel.linear_counter, 3);
            assert!(channel.linear_reload);
        }
        channel.write_register(0, 0x85);
        assert_eq!(channel.linear_counter, 3);
        channel.clock_quarter_frame();
        assert_eq!(channel.linear_counter, 5);

        channel.write_register(0, 2);
        channel.clock_quarter_frame();
        assert_eq!(channel.linear_counter, 2);
        assert!(!channel.linear_reload);
        channel.clock_quarter_frame();
        assert_eq!(channel.linear_counter, 1);
    }

    #[test]
    fn control_bit_alone_does_not_create_a_reload_request() {
        let mut channel = TriangleChannel::new();
        channel.write_register(0, 0x83);
        channel.clock_quarter_frame();
        assert_eq!(channel.linear_counter, 0);
        assert!(!channel.linear_reload);
    }

    #[test]
    fn half_frame_uses_bit_seven_to_halt_length_counter() {
        let mut channel = configured(3, 126);
        channel.clock_half_frame();
        assert_eq!(channel.length_counter, 9);
        channel.write_register(0, 0x83);
        channel.clock_half_frame();
        assert_eq!(channel.length_counter, 9);
        channel.write_register(0, 3);
        for _ in 0..12 {
            channel.clock_half_frame();
        }
        assert_eq!(channel.length_counter, 0);
        assert!(!channel.is_active());
    }

    #[test]
    fn timer_advances_wave_once_per_period_plus_one_cpu_cycles() {
        let mut channel = configured(0x83, 2);
        channel.clock_quarter_frame();
        channel.timer = 2;
        channel.sequence_step = 0;
        assert_eq!(channel.output(), 15);
        channel.clock_cpu_cycle();
        assert_eq!(channel.timer, 1);
        channel.clock_cpu_cycle();
        assert_eq!(channel.timer, 0);
        assert_eq!(channel.output(), 15);
        channel.clock_cpu_cycle();
        assert_eq!(channel.timer, 2);
        assert_eq!(channel.sequence_step, 1);
        assert_eq!(channel.output(), 14);
    }

    #[test]
    fn waveform_has_32_steps_and_small_periods_are_not_muted() {
        let expected = [
            15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
            11, 12, 13, 14, 15,
        ];
        for period in [0, 1, 7, 126] {
            let mut channel = configured(0x83, period);
            channel.clock_quarter_frame();
            channel.timer = period;
            channel.sequence_step = 0;
            for (step, output) in expected.into_iter().enumerate() {
                assert_eq!(channel.output(), output, "period={period}, step={step}");
                for _ in 0..=period {
                    channel.clock_cpu_cycle();
                }
            }
            assert_eq!(channel.sequence_step, 0);
            assert_eq!(channel.output(), 15);
        }
    }

    #[test]
    fn either_counter_stops_sequence_but_not_timer_or_held_output() {
        for (length, linear) in [(0, 3), (10, 0)] {
            let mut channel = configured(0x83, 2);
            channel.length_counter = length;
            channel.linear_counter = linear;
            channel.sequence_step = 5; // 当前输出为 10。
            channel.timer = 2;
            channel.clock_cpu_cycle();
            assert_eq!(channel.timer, 1);
            channel.clock_cpu_cycle();
            channel.clock_cpu_cycle();
            assert_eq!(channel.timer, 2);
            assert_eq!(channel.sequence_step, 5);
            assert_eq!(channel.output(), 10);

            channel.length_counter = 10;
            channel.linear_counter = 3;
            for _ in 0..3 {
                channel.clock_cpu_cycle();
            }
            assert_eq!(channel.sequence_step, 6);
            assert_eq!(channel.output(), 9);
        }
    }

    #[test]
    fn disabling_clears_length_but_does_not_force_output_to_zero() {
        let mut channel = configured(0x83, 2);
        channel.clock_quarter_frame();
        channel.sequence_step = 5;
        channel.set_enabled(false);
        assert_eq!(channel.length_counter, 0);
        assert_eq!(channel.output(), 10);
        for _ in 0..20 {
            channel.clock_cpu_cycle();
        }
        assert_eq!(channel.output(), 10);
        channel.set_enabled(true);
        assert_eq!(channel.length_counter, 0);
        channel.write_register(3, 0);
        assert_eq!(channel.length_counter, 10);
        assert_eq!(channel.sequence_step, 5);
    }

    #[test]
    fn apu_status_reports_length_not_whether_waveform_is_moving() {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 4);
        apu.write_register(0x4008, 0); // linear reload value = 0。
        apu.write_register(0x400A, 126);
        apu.write_register(0x400B, 0);
        apu.clock_quarter_frame();
        assert_eq!(apu.triangle.linear_counter, 0);
        assert_eq!(apu.read_status() & 4, 4);
        assert_eq!(apu.triangle_output(), 15);
        apu.write_register(0x4015, 0);
        assert_eq!(apu.read_status() & 4, 0);
        assert_eq!(apu.triangle_output(), 15);
    }

    #[test]
    fn apu_helper_clocks_reach_triangle_without_advancing_wave_phase() {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 4);
        apu.write_register(0x4008, 3);
        apu.write_register(0x400A, 126);
        apu.write_register(0x400B, 0);
        apu.clock_quarter_frame();
        apu.clock_half_frame();
        assert_eq!(apu.triangle.linear_counter, 3);
        assert_eq!(apu.triangle.length_counter, 9);
        assert_eq!(apu.triangle.sequence_step, 0);
    }

    #[test]
    fn apu_cpu_clock_drives_timer_and_existing_four_step_frame_events() {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 4);
        apu.write_register(0x4008, 3);
        apu.write_register(0x400A, 126);
        apu.write_register(0x400B, 0);
        let mut elapsed = 0;
        for (target_cycle, linear, length) in
            [(7457, 3, 10), (14913, 2, 9), (22371, 1, 9), (29829, 0, 8)]
        {
            while elapsed < target_cycle {
                apu.clock_cpu_cycle();
                elapsed += 1;
            }
            assert_eq!(apu.triangle.linear_counter, linear, "cycle={elapsed}");
            assert_eq!(apu.triangle.length_counter, length, "cycle={elapsed}");
            if target_cycle == 7457 {
                // 下一次到期在 7494；验证 timer 已一直运行，而非此时才启动。
                assert_eq!(apu.triangle.timer, 36);
            }
            if target_cycle == 14913 {
                // 7494..14860 共 59 次前进：59 % 32 = 27。
                assert_eq!(apu.triangle.sequence_step, 27);
                assert_eq!(apu.triangle_output(), 11);
            }
        }
        assert_eq!(apu.read_status() & 4, 4);
    }
}
