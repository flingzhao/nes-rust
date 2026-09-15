const CPU_RAM_SIZE: usize = 0x800;
// const NES_PPU_REG_ADDR: usize = 0x2000;
const CPU_RAM_MAPPED_END: u16 = 0x1FFF;
const CPU_RAM_MIRROR_MASK: u16 = 0x07FF;

pub struct Bus {
    ram: [u8; CPU_RAM_SIZE],
}

impl Bus {
    pub fn new() -> Self {
        Self {
            ram: [0; CPU_RAM_SIZE],
        }
    }

    pub fn cpu_read(&mut self, addr: u16) -> Result<u8, String> {
        if addr > CPU_RAM_MAPPED_END {
            return Err(format!("unimplemented address {addr:#06X}"));
        }
        let index = usize::from(addr & CPU_RAM_MIRROR_MASK);
        Ok(self.ram[index])
    }

    pub fn cpu_write(&mut self, addr: u16, data: u8) -> Result<(), String> {
        if addr > CPU_RAM_MAPPED_END {
            return Err(format!("unimplemented address {addr:#06X}"));
        }
        let index = usize::from(addr & CPU_RAM_MIRROR_MASK);
        self.ram[index] = data;
        Ok(())
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
}
