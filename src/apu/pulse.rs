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
            0x0001 => {}
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
    }

    pub(super) fn is_active(&self) -> bool {
        return self.length_counter > 0;
    }

    pub(super) fn output(&self) -> u8 {
        if !self.enabled || !self.is_active() {
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
