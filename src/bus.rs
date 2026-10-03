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

    #[cfg(test)]
    write_trace: Vec<(u16, u8)>,
}

impl Bus {
    pub fn new() -> Self {
        Self {
            ram: [0; CPU_RAM_SIZE],
            cartridge: None,
            #[cfg(test)]
            write_trace: Vec::new(),
            ppu: Ppu::new(),
            oam_dma_request: None,
            controllers: [Controller::new(), Controller::new()],
        }
    }

    pub fn insert_cartridge(&mut self, cartridge: Cartridge) {
        self.cartridge = Some(cartridge);
    }

    pub fn cpu_read(&mut self, addr: u16) -> Result<u8, String> {
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
            0x4015 => {
                // TODO(APU): 暂时报告无活动声道、无 APU IRQ。
                Ok(0)
            }
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
        } else {
            println!("unimplemented addr write: 0x{addr:04X}");
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn take_write_trace(&mut self) -> Vec<(u16, u8)> {
        std::mem::take(&mut self.write_trace)
    }

    pub fn take_oam_dma_request(&mut self) -> Option<u8> {
        self.oam_dma_request.take()
    }

    pub fn ppu(&self) -> &Ppu {
        &self.ppu
    }

    pub(crate) fn clock_cpu_cycle(&mut self) -> Result<[bool; 3], String> {
        // 调用三次 self.ppu.clock()。
        let mut levels = [false; 3];

        for level in &mut levels {
            self.ppu.clock(self.cartridge.as_ref())?;
            *level = self.ppu.nmi_line();
        }

        Ok(levels)
    }

    pub fn take_frame_ready(&mut self) -> bool {
        self.ppu.take_frame_ready()
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
        fn accepts_apu_writes_without_starting_dma() {
            let mut bus = Bus::new();

            for addr in 0x4000u16..=0x4013 {
                bus.cpu_write(addr, 0xFF).unwrap();
            }
            bus.cpu_write(0x4015, 0x1F).unwrap();
            bus.cpu_write(0x4017, 0x40).unwrap();

            assert_eq!(bus.cpu_read(0x4015).unwrap(), 0);
            assert_eq!(bus.take_oam_dma_request(), None);

            // 确认新增分支没有吞掉 OAM DMA。
            bus.cpu_write(0x4014, 0x02).unwrap();
            assert_eq!(bus.take_oam_dma_request(), Some(0x02));
            assert_eq!(bus.take_oam_dma_request(), None);
        }
    }
}
