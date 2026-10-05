pub mod frame_counter;
pub mod pulse;

use frame_counter::FrameCounter;
use pulse::PulseChannel;

pub struct Apu {
    pulses: [PulseChannel; 2],
    frame_counter: FrameCounter,
}

impl Apu {
    pub fn new() -> Self {
        let pulses = [PulseChannel::new(), PulseChannel::new()];
        Self {
            pulses: pulses,
            frame_counter: FrameCounter::new(),
        }
    }

    pub fn write_register(&mut self, addr: u16, value: u8) {
        match addr {
            0x4000..=0x4007 => {
                let offset = addr - 0x4000;
                let channel_index = usize::from(offset / 4);
                let register = (offset % 4) as u8;

                self.pulses[channel_index].write_register(register, value);
            }
            0x4015 => {
                self.pulses[0].set_enabled(value & 0x01 != 0);
                self.pulses[1].set_enabled(value & 0x02 != 0);
            }
            0x4008..=0x4013 | 0x4017 => {
                // TODO：其它声道和帧序列器。
            }
            _ => {
                // 不属于本接口处理的 APU 寄存器。
                // $4014/$4016 应由 Bus 分别处理 DMA 和手柄。
            }
        }
    }

    pub fn read_status(&mut self) -> u8 {
        let pulse1 = u8::from(self.pulses[0].is_active());
        let pulse2 = u8::from(self.pulses[1].is_active());

        pulse1 | (pulse2 << 1)
    }

    pub fn clock_cpu_cycle(&mut self) {
        self.pulses[0].clock_cpu_cycle();
        self.pulses[1].clock_cpu_cycle();

        let events = self.frame_counter.clock_cpu_cycle();

        if events.half {
            self.pulses[0].clock_half_frame();
            self.pulses[1].clock_half_frame();
        }

        if events.quarter {
            self.pulses[0].clock_quarter_frame();
            self.pulses[1].clock_quarter_frame();
        }
    }
    pub fn clock_half_frame(&mut self) {
        self.pulses[0].clock_half_frame();
        self.pulses[1].clock_half_frame();
    }

    pub fn clock_quarter_frame(&mut self) {
        self.pulses[0].clock_quarter_frame();
        self.pulses[1].clock_quarter_frame();
    }

    // 返回两个声道各自的原始输出，不做混音。
    pub fn pulse_outputs(&self) -> [u8; 2] {
        let volume0 = self.pulses[0].output();
        let volume1 = self.pulses[1].output();

        [volume0, volume1]
    }
}
