pub mod dmc;
pub mod frame_counter;
pub mod noise;
pub mod pulse;
pub mod triangle;

use dmc::DmcChannel;
use frame_counter::FrameCounter;
use noise::NoiseChannel;
use pulse::PulseChannel;
use triangle::TriangleChannel;

pub struct Apu {
    pulses: [PulseChannel; 2],
    frame_counter: FrameCounter,
    triangle: TriangleChannel,
    noise: NoiseChannel,
    dmc: DmcChannel,
}

impl Apu {
    pub fn new() -> Self {
        Self {
            pulses: std::array::from_fn(|index| PulseChannel::new_with_channel(index == 0)),
            frame_counter: FrameCounter::new(),
            triangle: TriangleChannel::new(),
            noise: NoiseChannel::new(),
            dmc: DmcChannel::new(),
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
                self.triangle.set_enabled(value & 0x04 != 0);
                self.noise.set_enabled(value & 0x08 != 0);
                self.dmc.set_enabled(value & 0x10 != 0);
            }
            0x4008..=0x400B => {
                let register = (addr - 0x4008) as u8;
                self.triangle.write_register(register, value);
            }
            0x400C..=0x400F => {
                let register = (addr - 0x400C) as u8;
                self.noise.write_register(register, value);
            }
            0x4010..=0x4013 => {
                let register = (addr - 0x4010) as u8;
                self.dmc.write_register(register, value);
            }
            0x4017 => {
                self.frame_counter.write_register(value);
            }
            _ => {
                // 不属于本接口处理的 APU 寄存器。
                // $4014/$4016 应由 Bus 分别处理 DMA 和手柄。
            }
        }
    }

    pub fn read_status(&mut self) -> u8 {
        let pulse1 = u8::from(self.pulses[0].is_active());
        let pulse2 = u8::from(self.pulses[1].is_active()) << 1;

        let irq_bit = u8::from(self.frame_counter.irq_line()) << 6;

        self.frame_counter.clear_frame_irq();

        let triangle_bit = u8::from(self.triangle.is_active()) << 2;

        let noise_bit = u8::from(self.noise.is_active()) << 3;

        let dmc_active_bit = u8::from(self.dmc.is_active()) << 4;
        let dmc_irq_bit = u8::from(self.dmc.irq_pending()) << 7;

        dmc_irq_bit | irq_bit | dmc_active_bit | noise_bit | triangle_bit | pulse2 | pulse1
    }

    pub fn clock_cpu_cycle(&mut self) {
        self.pulses[0].clock_cpu_cycle();
        self.pulses[1].clock_cpu_cycle();
        self.triangle.clock_cpu_cycle();
        self.noise.clock_cpu_cycle();
        self.dmc.clock_cpu_cycle();

        let events = self.frame_counter.clock_cpu_cycle();

        if events.half {
            self.clock_half_frame();
            // self.pulses[0].clock_half_frame();
            // self.pulses[1].clock_half_frame();
        }

        if events.quarter {
            self.clock_quarter_frame();
            // self.pulses[0].clock_quarter_frame();
            // self.pulses[1].clock_quarter_frame();
        }
    }

    pub fn clock_half_frame(&mut self) {
        self.pulses[0].clock_half_frame();
        self.pulses[1].clock_half_frame();
        self.triangle.clock_half_frame();
        self.noise.clock_half_frame();
    }

    pub fn clock_quarter_frame(&mut self) {
        self.pulses[0].clock_quarter_frame();
        self.pulses[1].clock_quarter_frame();
        self.triangle.clock_quarter_frame();
        self.noise.clock_quarter_frame();
    }

    // 返回两个声道各自的原始输出，不做混音。
    pub fn pulse_outputs(&self) -> [u8; 2] {
        let volume0 = self.pulses[0].output();
        let volume1 = self.pulses[1].output();

        [volume0, volume1]
    }

    pub fn irq_line(&self) -> bool {
        self.frame_counter.irq_line() | self.dmc.irq_pending()
    }

    pub fn triangle_output(&self) -> u8 {
        self.triangle.output()
    }

    pub fn noise_output(&self) -> u8 {
        self.noise.output()
    }

    pub fn dmc_output(&self) -> u8 {
        self.dmc.output()
    }

    pub(crate) fn complete_dmc_sample_read(&mut self, value: u8) -> Result<(), String> {
        self.dmc.complete_sample_read(value)
    }

    pub(crate) fn dmc_next_sample_address(&self) -> Option<u16> {
        self.dmc.next_sample_address()
    }

    pub(crate) fn dmc_active(&self) -> bool {
        self.dmc.is_active()
    }
}
