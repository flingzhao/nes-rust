const DUTY_SEQUENCES: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0],
    [0, 1, 1, 0, 0, 0, 0, 0],
    [0, 1, 1, 1, 1, 0, 0, 0],
    [1, 0, 0, 1, 1, 1, 1, 1],
];

pub struct PulseOscillator {
    duty: u8,
    timer_period: u16,
    timer: u16,
    sequence_step: u8,
    cpu_phase: bool,
    volume: u8,
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
        if self.timer_period < 8 {
            return 0;
        }

        let value = DUTY_SEQUENCES[usize::from(self.duty)][usize::from(self.sequence_step)];
        match value {
            0 => 0,
            1 => self.volume,
            _ => 0,
        }
    }
}
