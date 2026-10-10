use crate::bus::Bus;

pub trait CpuBus {
    fn cpu_read(&mut self, addr: u16) -> Result<u8, String>;

    fn cpu_write(&mut self, addr: u16, value: u8) -> Result<(), String>;
}

impl CpuBus for Bus {
    fn cpu_read(&mut self, addr: u16) -> Result<u8, String> {
        Bus::cpu_read(self, addr)
    }

    fn cpu_write(&mut self, addr: u16, value: u8) -> Result<(), String> {
        Bus::cpu_write(self, addr, value)
    }
}
