const DUTY_SEQUENCES: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

pub struct PulseOscillator {
    duty: u8,
    timer_period: u16,
    timer: u16,
    sequence_step: u8,
    cpu_phase: bool,
    volume: u8,
}

pub(super) struct PulseChannel {
    oscillator: PulseOscillator, // 当前波形输出等级
    enabled: bool,               // $4015 对应的使能位, 是否允许装载长度
    length_counter: u8,          // $4003 当前剩余长度计数
    control: u8,                 // $4000 或 $4004 的原始字节
    envelope_start: bool,
    envelope_divider: u8,
    envelope_decay: u8,
    is_pulse_one: bool, // 是否为声道1，决定减法是否额外减1。
    sweep_register: u8, // 最近写入的 EPPP NSSS。
    sweep_divider: u8,  // 按 half-frame 推进的当前倒计数。
    sweep_reload: bool, // 下一次 half-frame 是否重装倒计数。
}

impl PulseOscillator {
    pub fn new(duty: u8, timer_period: u16, volume: u8) -> Result<Self, String> {
        if !(0..=3).contains(&duty) {
            return Err(format!(
                "pulse oscillator only support 0-3 duties, got {duty}"
            ));
        }
        if !(0..=0x7FF).contains(&timer_period) {
            return Err(format!(
                "pulse oscillator only support 0-0x7FF timer_period, got {timer_period:#04X}"
            ));
        }
        if !(0..=15).contains(&volume) {
            return Err(format!(
                "pulse oscillator only support 0-15 volume, got {volume}"
            ));
        }
        Ok(Self {
            duty,
            timer_period,
            sequence_step: 0,
            timer: timer_period,
            cpu_phase: false,
            volume,
        })
    }

    pub fn clock_cpu_cycle(&mut self) {
        self.cpu_phase = !self.cpu_phase;
        if self.cpu_phase {
            return;
        }
        if self.timer > 0 {
            self.timer -= 1;
        } else {
            // 重装 timer_period, sequence_step 前进一格
            self.sequence_step = (self.sequence_step + 1) % 8;
            self.timer = self.timer_period;
        }
    }

    pub fn output(&self) -> u8 {
        self.output_with_volume(self.volume)
    }

    pub(super) fn output_with_volume(&self, volume: u8) -> u8 {
        if self.timer_period < 8 {
            return 0;
        }

        let value = DUTY_SEQUENCES[usize::from(self.duty)][usize::from(self.sequence_step)];
        match value {
            0 => 0,
            1 => volume,
            _ => 0,
        }
    }
}

impl PulseChannel {
    pub(super) fn new() -> Self {
        Self::new_with_channel(true)
    }

    pub(super) fn new_with_channel(is_pulse_one: bool) -> Self {
        // 原来的初始化，加上上述字段。
        let oscillator = PulseOscillator::new(0, 0, 0).unwrap();
        let enabled = false;
        let length_counter = 0;
        let control = 0;
        Self {
            oscillator,
            enabled,
            length_counter,
            control,
            envelope_start: false,
            envelope_divider: 0,
            envelope_decay: 0,
            is_pulse_one,
            sweep_register: 0,
            sweep_divider: 0,
            sweep_reload: false,
        }
    }

    // register 为 0、1、2、3，对应一组声道寄存器内的偏移。
    pub(super) fn write_register(&mut self, register: u8, value: u8) {
        match register {
            0x0000 => {
                // bit: 7 6   5   4   3 2 1 0
                //      D D   H   C   V V V V
                // - DD：波形编号，对应你的 duty。
                // - H：是否暂停长度倒计数；以后也用于包络循环。
                // - C：为 1 时使用固定音量。
                // - VVVV：固定音量模式下的音量。
                self.control = value;
                self.oscillator.duty = value >> 6;
                // self.oscillator.volume = value & 0x0F;
            }
            0x0001 => {
                self.sweep_register = value;
                self.sweep_reload = true;
            }
            0x0002 => {
                self.oscillator.timer_period =
                    (self.oscillator.timer_period & 0x0700) | u16::from(value);
            }
            0x0003 => {
                // bit: 7 6 5 4 3  2 1 0
                //      L L L L L  T T T
                //  - TTT：定时参数的高 3 位。
                //  - LLLLL：长度查表编号。
                self.oscillator.timer_period =
                    (self.oscillator.timer_period & 0x00FF) | u16::from(value & 0x07) << 8;
                self.oscillator.sequence_step = 0;
                if self.enabled {
                    let index = usize::from(value >> 3);
                    self.length_counter = LENGTH_TABLE[index];
                }
                self.envelope_start = true;
            }
            _ => unreachable!("apu register only support 0/1/2/3"),
        }
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;

        if !enabled {
            self.length_counter = 0;
        }
    }

    pub(super) fn clock_cpu_cycle(&mut self) {
        self.oscillator.clock_cpu_cycle();
    }

    pub(super) fn clock_half_frame(&mut self) {
        if self.control & 0x20 == 0 && self.length_counter > 0 {
            self.length_counter -= 1;
        }

        // bit：7  6 5 4  3  2 1 0
        //      E  P P P  N  S S S
        // 同时满足:
        // 1. sweep_divider == 0；
        // 2. E=1；
        // 3. S != 0；
        // 4. 没有触发静音条件；
        // 才把当前周期改为目标周期。
        let shift = self.sweep_register & 0x07;
        if self.sweep_divider == 0
            && self.sweep_register & 0x80 != 0
            && shift != 0
            && !self.sweep_muted()
        {
            self.oscillator.timer_period = self.sweep_target_period();
        }

        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_reload = false;
            self.sweep_divider = (self.sweep_register >> 4) & 0x07;
        } else {
            self.sweep_divider -= 1;
        }
    }

    fn sweep_target_period(&self) -> u16 {
        let shift = self.sweep_register & 0x07;
        let adjust = self.oscillator.timer_period >> shift;
        let target_period = match self.sweep_register & 0x08 != 0 {
            true => self
                .oscillator
                .timer_period
                .saturating_sub(adjust)
                .saturating_sub(u16::from(self.is_pulse_one)),
            false => self.oscillator.timer_period.saturating_add(adjust),
        };
        target_period
    }

    fn sweep_muted(&self) -> bool {
        self.oscillator.timer_period < 8 || self.sweep_target_period() > 0x7FF
    }

    pub(super) fn is_active(&self) -> bool {
        return self.length_counter > 0;
    }

    pub(super) fn output(&self) -> u8 {
        if !self.enabled || !self.is_active() || self.sweep_muted() {
            return 0;
        }

        let volume = if self.control & 0x10 != 0 {
            self.control & 0x0F // C=1：固定音量。
        } else {
            self.envelope_decay // C=0：当前包络音量
        };

        self.oscillator.output_with_volume(volume)
    }

    pub(super) fn clock_quarter_frame(&mut self) {
        if self.envelope_start {
            self.envelope_start = false;
            self.envelope_decay = 15;
            self.envelope_divider = self.control & 0x0F;
            return;
        }

        if self.envelope_divider > 0 {
            self.envelope_divider -= 1;
        } else {
            self.envelope_divider = self.control & 0x0F;
            if self.envelope_decay > 0 {
                self.envelope_decay -= 1;
            } else if self.control & 0x20 != 0 {
                self.envelope_decay = 15;
            }
        }
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::apu::Apu;

    fn channel(is_pulse_one: bool, period: u16) -> PulseChannel {
        let mut channel = PulseChannel::new_with_channel(is_pulse_one);
        channel.set_enabled(true);
        channel.write_register(0, 0x3F); // 固定音量15，暂停长度递减。
        channel.write_register(2, period as u8);
        channel.write_register(3, ((period >> 8) & 7) as u8);
        // 本组输出门控测试固定在 duty=0 的高电平位置，排除波形相位干扰。
        channel.oscillator.sequence_step = 1;
        channel
    }

    #[test]
    fn target_calculation_distinguishes_the_two_channels() {
        for (is_one, expected_negative) in [(true, 191), (false, 192)] {
            let mut ch = channel(is_one, 256);
            ch.write_register(1, 0x82); // 加法，shift=2。
            assert_eq!(ch.sweep_target_period(), 320);
            ch.write_register(1, 0x8A); // 减法，shift=2。
            assert_eq!(ch.sweep_target_period(), expected_negative);
            assert_eq!(ch.oscillator.timer_period, 256, "计算目标不代表写回周期");
        }
    }

    #[test]
    fn writes_defer_reload_until_half_frame_and_period_is_p_plus_one() {
        let mut ch = channel(true, 256);
        ch.write_register(1, 0xA2); // E=1, P=2, N=0, S=2。
        assert_eq!(ch.oscillator.timer_period, 256);
        assert_eq!(ch.sweep_divider, 0);
        assert!(ch.sweep_reload);

        for (period, divider) in [(320, 2), (320, 1), (320, 0), (400, 2)] {
            ch.clock_half_frame();
            assert_eq!(ch.oscillator.timer_period, period);
            assert_eq!(ch.sweep_divider, divider);
            assert!(!ch.sweep_reload);
        }
    }

    #[test]
    fn negative_sweep_updates_the_actual_period() {
        for (is_one, expected) in [(true, 191), (false, 192)] {
            let mut ch = channel(is_one, 256);
            ch.write_register(1, 0x8A);
            ch.clock_half_frame();
            assert_eq!(ch.oscillator.timer_period, expected);
        }
    }

    #[test]
    fn reload_with_nonzero_divider_does_not_adjust_pitch() {
        let mut ch = channel(true, 256);
        ch.sweep_divider = 1;
        ch.write_register(1, 0xA2);
        assert_eq!(
            ch.sweep_divider, 1,
            "写入时只设置 reload，不直接重装 divider"
        );
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 256);
        assert_eq!(ch.sweep_divider, 2);
        assert!(!ch.sweep_reload);
        ch.clock_half_frame(); // 2 -> 1。
        ch.clock_half_frame(); // 1 -> 0；此时不能提前更新。
        assert_eq!(ch.oscillator.timer_period, 256);
        ch.clock_half_frame(); // 进入调用时已经为0，才更新。
        assert_eq!(ch.oscillator.timer_period, 320);
    }

    #[test]
    fn reload_with_zero_divider_still_adjusts_pitch_before_reloading() {
        let mut ch = channel(true, 256);
        ch.write_register(1, 0xF2); // P=7。
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 320);
        assert_eq!(ch.sweep_divider, 7);
        assert!(!ch.sweep_reload);
    }

    #[test]
    fn disabled_sweep_keeps_period_but_still_clocks_divider() {
        let mut ch = channel(true, 256);
        ch.write_register(1, 0x22); // E=0, P=2, S=2。
        for expected_divider in [2, 1, 0, 2] {
            ch.clock_half_frame();
            assert_eq!(ch.sweep_divider, expected_divider);
            assert_eq!(ch.oscillator.timer_period, 256);
        }
    }

    #[test]
    fn zero_shift_never_writes_back_the_target() {
        for is_one in [true, false] {
            let mut ch = channel(is_one, 256);
            ch.write_register(1, 0x88); // E=1，N=1，但S=0。
            assert_eq!(ch.sweep_target_period(), 0);
            for _ in 0..8 {
                ch.clock_half_frame();
            }
            assert_eq!(ch.oscillator.timer_period, 256);
            assert_eq!(ch.output(), 15);
        }
    }

    #[test]
    fn overflow_mutes_even_when_sweep_is_disabled_and_length_is_active() {
        let mut ch = channel(true, 0x600);
        ch.write_register(1, 0x01); // E=0，但目标仍为0x600+0x300。
        assert_eq!(ch.sweep_target_period(), 0x900);
        assert!(ch.is_active(), "静音不能清空长度计数器");
        assert_eq!(ch.output(), 0);
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 0x600);
        assert_eq!(ch.sweep_divider, 0);
    }

    #[test]
    fn muting_reacts_immediately_to_register_and_period_changes() {
        let mut ch = channel(true, 0x400);
        ch.write_register(1, 0x00); // E=0,S=0，但正向目标为0x800。
        assert_eq!(ch.output(), 0);
        ch.write_register(1, 0x08); // N=1，目标不再越界。
        assert_eq!(ch.output(), 15, "不等待下一次 half-frame 才解除静音");
        ch.write_register(1, 0x00);
        assert_eq!(ch.output(), 0);
        ch.write_register(2, 0xFF);
        ch.write_register(3, 0x03); // 当前周期变为0x3FF，目标0x7FE。
        ch.oscillator.sequence_step = 1;
        assert_eq!(ch.output(), 15);
    }

    #[test]
    fn current_period_below_eight_mutes_and_prevents_sweep_updates() {
        let mut ch = channel(true, 7);
        ch.write_register(1, 0x81);
        assert_eq!(ch.sweep_target_period(), 10);
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 7);
        assert_eq!(ch.output(), 0);
    }

    #[test]
    fn valid_update_can_result_in_a_period_below_eight() {
        let mut ch = channel(true, 8);
        ch.write_register(1, 0x8B); // 8 - (8 >> 3) - 1 = 6。
        assert_eq!(
            ch.output(),
            15,
            "更新前当前周期为8，不能按目标小于8提前静音"
        );
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 6);
        assert_eq!(ch.output(), 0);
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 6);
    }

    #[test]
    fn new_target_can_mute_immediately_after_a_valid_update() {
        let mut ch = channel(true, 0x500);
        ch.write_register(1, 0x81);
        assert_eq!(ch.sweep_target_period(), 0x780);
        assert_eq!(ch.output(), 15);
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 0x780);
        assert_eq!(ch.sweep_target_period(), 0xB40);
        assert_eq!(ch.output(), 0);
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 0x780, "越界目标不得写回");
    }

    #[test]
    fn sweep_does_not_restart_timer_phase_envelope_or_length() {
        let mut ch = channel(true, 256);
        ch.oscillator.timer = 17;
        ch.oscillator.sequence_step = 5;
        ch.envelope_start = false;
        ch.envelope_decay = 9;
        ch.envelope_divider = 4;
        let length = ch.length_counter;
        ch.write_register(1, 0x82);
        ch.clock_half_frame();
        assert_eq!(ch.oscillator.timer_period, 320);
        assert_eq!(ch.oscillator.timer, 17);
        assert_eq!(ch.oscillator.sequence_step, 5);
        assert!(!ch.envelope_start);
        assert_eq!(ch.envelope_decay, 9);
        assert_eq!(ch.envelope_divider, 4);
        assert_eq!(ch.length_counter, length);
    }

    #[test]
    fn quarter_frame_does_not_clock_sweep() {
        let mut ch = channel(true, 256);
        ch.write_register(1, 0xA2);
        ch.clock_quarter_frame();
        assert_eq!(ch.oscillator.timer_period, 256);
        assert_eq!(ch.sweep_divider, 0);
        assert!(ch.sweep_reload);
    }

    #[test]
    fn apu_half_frame_clocks_sweep_with_correct_channel_identity() {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 3);
        for base in [0x4000u16, 0x4004] {
            apu.write_register(base, 0x3F);
            apu.write_register(base + 2, 0x00);
            apu.write_register(base + 3, 0x01); // 周期256。
            apu.write_register(base + 1, 0x8A);
        }
        for _ in 0..14_912 {
            apu.clock_cpu_cycle();
        }
        assert_eq!(apu.pulses[0].oscillator.timer_period, 256);
        assert_eq!(apu.pulses[1].oscillator.timer_period, 256);
        apu.clock_cpu_cycle();
        assert_eq!(apu.pulses[0].oscillator.timer_period, 191);
        assert_eq!(apu.pulses[1].oscillator.timer_period, 192);
    }
}
