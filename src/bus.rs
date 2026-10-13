use crate::apu::Apu;
use crate::audio::{AUDIO_SAMPLE_RATE, AudioFilter, CpuRateSampler};
use crate::cartridge::Cartridge;
use crate::controller::{Button, Controller};
use crate::ppu::Ppu;

const CPU_RAM_SIZE: usize = 0x800;
// const NES_PPU_REG_ADDR: usize = 0x2000;
const CPU_RAM_MAPPED_END: u16 = 0x1FFF;
const CPU_RAM_MIRROR_MASK: u16 = 0x07FF;

pub struct Bus {
    ram: [u8; CPU_RAM_SIZE],
    cartridge: Option<Cartridge>,
    ppu: Ppu,
    oam_dma_request: Option<u8>,
    controllers: [Controller; 2],
    apu: Apu,
    audio_sampler: CpuRateSampler,
    audio_samples: Vec<f32>,
    audio_filter: AudioFilter,

    #[cfg(test)]
    write_trace: Vec<(u16, u8)>,
    #[cfg(test)]
    read_trace: Vec<u16>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            ram: [0; CPU_RAM_SIZE],
            cartridge: None,
            #[cfg(test)]
            write_trace: Vec::new(),
            #[cfg(test)]
            read_trace: Vec::new(),
            ppu: Ppu::new(),
            apu: Apu::new(),
            oam_dma_request: None,
            controllers: [Controller::new(), Controller::new()],
            audio_sampler: CpuRateSampler::new(
                crate::audio::NTSC_CPU_HZ,
                crate::audio::AUDIO_SAMPLE_RATE,
            )
            .expect(""),
            audio_samples: Vec::<f32>::new(),
            audio_filter: AudioFilter::new(crate::audio::AUDIO_SAMPLE_RATE).expect(""),
        }
    }

    pub fn insert_cartridge(&mut self, cartridge: Cartridge) {
        self.cartridge = Some(cartridge);
    }

    pub fn cpu_read(&mut self, addr: u16) -> Result<u8, String> {
        #[cfg(test)]
        self.read_trace.push(addr);
        match addr {
            0x0000..=0x1FFF => {
                let index = usize::from(addr & CPU_RAM_MIRROR_MASK);
                Ok(self.ram[index])
            }
            0x2000..=0x3FFF => {
                let cartridge = self
                    .cartridge
                    .as_mut()
                    .ok_or_else(|| "No cartridge inserted".to_string())?;
                Ok(self.ppu.cpu_read(addr, cartridge)?)
            }
            0x4015 => Ok(self.apu.read_status()),
            0x4016 => Ok(self.controllers[0].read()),
            0x4017 => Ok(self.controllers[1].read()),
            0x8000..=0xFFFF => {
                let cartridge = self
                    .cartridge
                    .as_ref()
                    .ok_or_else(|| "No cartridge inserted".to_string())?;

                cartridge.cpu_read(addr)
            }
            _ => Err(format!("unimplemented addr access: {addr:#04X}")),
            // _ => Ok(0),
        }
    }

    pub fn cpu_write(&mut self, addr: u16, data: u8) -> Result<(), String> {
        #[cfg(test)]
        self.write_trace.push((addr, data));
        if addr > 0x7FFF {
            return Err(format!("unimplemented address {addr:#06X}"));
        }
        if addr <= CPU_RAM_MAPPED_END {
            let index = usize::from(addr & CPU_RAM_MIRROR_MASK);
            self.ram[index] = data;
        } else if addr > CPU_RAM_MAPPED_END && addr <= 0x3FFF {
            let cartridge = self
                .cartridge
                .as_mut()
                .ok_or_else(|| "No cartridge inserted".to_string())?;
            self.ppu.cpu_write(addr, data, cartridge)?;
        } else if addr == 0x4016 {
            for controller in &mut self.controllers {
                controller.write_strobe(data);
            }
        } else if addr == 0x4014 {
            self.oam_dma_request = Some(data);
        } else if matches!(addr, 0x4000..=0x4013 | 0x4015 | 0x4017) {
            self.apu.write_register(addr, data);
        } else {
            println!("unimplemented addr write: 0x{addr:04X}");
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn take_write_trace(&mut self) -> Vec<(u16, u8)> {
        std::mem::take(&mut self.write_trace)
    }

    #[cfg(test)]
    pub(crate) fn take_read_trace(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.read_trace)
    }

    pub fn take_oam_dma_request(&mut self) -> Option<u8> {
        self.oam_dma_request.take()
    }

    pub fn ppu(&self) -> &Ppu {
        &self.ppu
    }

    pub fn apu(&mut self) -> &mut Apu {
        &mut self.apu
    }

    pub(crate) fn dmc_output(&self) -> u8 {
        self.apu.dmc_output()
    }

    pub(crate) fn clock_cpu_cycle(&mut self) -> Result<[bool; 3], String> {
        // 调用三次 self.ppu.clock()。
        let mut levels = [false; 3];

        for level in &mut levels {
            self.ppu.clock(self.cartridge.as_ref())?;
            *level = self.ppu.nmi_line();
        }

        self.apu.clock_cpu_cycle();

        let level = self.apu.mixed_output();
        if let Some(sample) = self.audio_sampler.push_cycle(level) {
            let filtered = self.audio_filter.process(sample);
            self.audio_samples.push(filtered);
        }

        Ok(levels)
    }

    pub fn take_frame_ready(&mut self) -> bool {
        self.ppu.take_frame_ready()
    }

    pub(crate) fn take_audio_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio_samples)
    }

    // impl Bus
    pub fn set_controller_button(
        &mut self,
        port: usize,
        button: Button,
        pressed: bool,
    ) -> Result<(), String> {
        let controller = self
            .controllers
            .get_mut(port)
            .ok_or_else(|| format!("Invalid controller port: {port}"))?;

        controller.set_button(button, pressed);
        Ok(())
    }

    pub fn release_all_buttons(&mut self) {
        for controller in &mut self.controllers {
            controller.release_all();
        }
    }

    pub fn irq_line(&self) -> bool {
        self.apu.irq_line()
    }

    pub(crate) fn dmc_next_sample_address(&self) -> Option<u16> {
        self.apu.dmc_next_sample_address()
    }

    pub(crate) fn dmc_active(&self) -> bool {
        self.apu.dmc_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_ram() {
        let mut bus = Bus::new();

        bus.cpu_write(0x0002, 42).unwrap();

        assert_eq!(bus.cpu_read(0x0002).unwrap(), 42);
        assert_eq!(bus.cpu_read(0x0003).unwrap(), 0);
    }

    #[test]
    fn mirrors_ram_in_both_directions() {
        let mut bus = Bus::new();

        bus.cpu_write(0x0002, 42).unwrap();

        for addr in [0x0002, 0x0802, 0x1002, 0x1802] {
            assert_eq!(bus.cpu_read(addr).unwrap(), 42);
        }

        bus.cpu_write(0x1802, 99).unwrap();
        assert_eq!(bus.cpu_read(0x0002).unwrap(), 99);
    }

    #[test]
    fn handles_last_ram_address() {
        let mut bus = Bus::new();

        bus.cpu_write(0x1FFF, 7).unwrap();
        assert_eq!(bus.cpu_read(0x07FF).unwrap(), 7);
    }

    #[test]
    fn rejects_unimplemented_addresses() {
        let mut bus = Bus::new();

        assert!(bus.cpu_read(0x2000).is_err());
        assert!(bus.cpu_write(0x8000, 1).is_err());
    }

    #[cfg(test)]
    mod silent_apu_tests {
        use super::*;
        #[test]
        fn accepts_apu_writes_without_starting_oam_dma() {
            let mut bus = Bus::new();

            // 此时其他四个声道尚未使能，长度写入不会装载长度计数器。
            // DMC 的样本地址、样本长度配置仍然会保存。
            for addr in 0x4000u16..=0x4013 {
                bus.cpu_write(addr, 0xFF).unwrap();
            }

            // 启动 DMC，bytes_remaining 装载为 4081。
            bus.cpu_write(0x4015, 0x1F).unwrap();
            bus.cpu_write(0x4017, 0x40).unwrap();

            // 只有 DMC active（bit 4）为一。
            assert_eq!(bus.cpu_read(0x4015).unwrap(), 0x10);
            assert_eq!(bus.take_oam_dma_request(), None);

            // 关闭 DMC 读取，active 位应清零。
            bus.cpu_write(0x4015, 0x0F).unwrap();
            assert_eq!(bus.cpu_read(0x4015).unwrap(), 0);

            // APU 分支不能吞掉 $4014 的 OAM DMA 写入。
            bus.cpu_write(0x4014, 0x02).unwrap();
            assert_eq!(bus.take_oam_dma_request(), Some(0x02));
            assert_eq!(bus.take_oam_dma_request(), None);
        }
    }

    #[cfg(test)]
    mod apu_register_bus_tests {
        use super::*;

        #[test]
        fn bus_routes_pulse_registers_and_clocks_apu_once_per_cpu_cycle() {
            let mut bus = Bus::new();

            bus.cpu_write(0x4015, 1).unwrap();
            bus.cpu_write(0x4000, 0x1A).unwrap();
            bus.cpu_write(0x4002, 8).unwrap();
            bus.cpu_write(0x4003, 0x18).unwrap();

            assert_eq!(bus.cpu_read(0x4015).unwrap(), 1);
            assert_eq!(bus.apu.pulse_outputs(), [0, 0]);

            bus.clock_cpu_cycle().unwrap();
            assert_eq!(bus.apu.pulse_outputs(), [0, 0]);

            bus.clock_cpu_cycle().unwrap();
            assert_eq!(bus.apu.pulse_outputs(), [10, 0]);

            bus.cpu_write(0x4015, 0).unwrap();
            assert_eq!(bus.cpu_read(0x4015).unwrap(), 0);
            assert_eq!(bus.apu.pulse_outputs(), [0, 0]);
        }

        #[test]
        fn apu_routing_preserves_dma_and_second_controller_reads() {
            let mut bus = Bus::new();

            bus.set_controller_button(1, Button::A, true).unwrap();
            bus.cpu_write(0x4016, 1).unwrap();

            // 写 $4017 属于 APU，读 $4017 仍然属于第二个手柄。
            bus.cpu_write(0x4017, 0x40).unwrap();
            assert_eq!(bus.cpu_read(0x4017).unwrap() & 1, 1);

            bus.cpu_write(0x4000, 0x1A).unwrap();
            assert_eq!(bus.take_oam_dma_request(), None);

            bus.cpu_write(0x4014, 0x02).unwrap();
            assert_eq!(bus.take_oam_dma_request(), Some(0x02));
        }
    }
}
