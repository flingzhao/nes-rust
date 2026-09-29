use nes::bus::Bus;
use nes::cartridge::Cartridge;

fn make_cartridge(prg_banks: u8) -> Cartridge {
    let prg_size = usize::from(prg_banks) * 16 * 1024;
    let mut bytes = vec![0; 16 + prg_size];

    bytes[..4].copy_from_slice(b"NES\x1A");
    bytes[4] = prg_banks;
    bytes[5] = 0; // use chr ram

    bytes[16] = 0x11; // prg_rom[0] = 0x11
    bytes[16 + 0x3FFF] = 0x22; // prg_rom[16383] = 0x22

    if prg_banks == 2 {
        bytes[16 + 0x4000] = 0x33; // prg_rom[16384] = 0x33
        bytes[16 + 0x7FFF] = 0x44; // prg_rom[32767] = 0x44
    }
    Cartridge::try_from(bytes.as_slice()).unwrap()
}

#[test]
fn mirrors_16_kib_prg_rom() {
    let mut bus = Bus::new();
    bus.insert_cartridge(make_cartridge(1));

    assert_eq!(bus.cpu_read(0x8000).unwrap(), 0x11);
    assert_eq!(bus.cpu_read(0xBFFF).unwrap(), 0x22);
    assert_eq!(bus.cpu_read(0xC000).unwrap(), 0x11);
    assert_eq!(bus.cpu_read(0xFFFF).unwrap(), 0x22);
}

#[test]
fn maps_both_halves_of_32_kib_prg_rom() {
    let mut bus = Bus::new();
    bus.insert_cartridge(make_cartridge(2));

    assert_eq!(bus.cpu_read(0x8000).unwrap(), 0x11);
    assert_eq!(bus.cpu_read(0xBFFF).unwrap(), 0x22);
    assert_eq!(bus.cpu_read(0xC000).unwrap(), 0x33);
    assert_eq!(bus.cpu_read(0xFFFF).unwrap(), 0x44);
}

#[test]
fn rejects_prg_reads_without_cartridge() {
    let mut bus = Bus::new();

    assert!(bus.cpu_read(0x8000).is_err());
}

#[test]
fn ram_still_works_and_rom_is_not_modified() {
    let mut bus = Bus::new();
    bus.insert_cartridge(make_cartridge(1));

    bus.cpu_write(0x0002, 0x55).unwrap();
    assert_eq!(bus.cpu_read(0x1802).unwrap(), 0x55);

    assert!(bus.cpu_write(0x8000, 0xFF).is_err());
    assert_eq!(bus.cpu_read(0x8000).unwrap(), 0x11);
}

#[test]
fn cartridge_rejects_addresses_outside_prg_region() {
    let cartridge = make_cartridge(1);

    assert!(cartridge.cpu_read(0x7FFF).is_err());
}

#[test]
fn cartridge_does_not_capture_unimplemented_bus_addresses() {
    let mut bus = Bus::new();
    bus.insert_cartridge(make_cartridge(1));

    for addr in [0x4016, 0x6000, 0x7FFF] {
        assert!(
            bus.cpu_read(addr).is_err(),
            "Address {addr:#06X} should remain unsupported for now"
        );
    }

    // 确保错误读取没有影响合法的卡带访问。
    assert_eq!(bus.cpu_read(0x8000).unwrap(), 0x11);
}
