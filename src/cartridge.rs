#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirroring {
    Horizontal,
    Vertical,
    FourScreen,
}

#[derive(Debug, PartialEq, Eq)]
pub struct InesHeader {
    pub prg_rom_size: usize,
    pub chr_rom_size: usize,
    pub mapper_id: u8,
    pub mirroring: Mirroring,
    pub has_battery: bool,
    pub has_trainer: bool,
}
/*
 * flags6
 * 76543210
 * ||||||||
 * |||||||+- Nametable arrangement: 0: vertical arrangement ("horizontal mirrored") (CIRAM A10 = PPU A11)
 * |||||||                          1: horizontal arrangement ("vertically mirrored") (CIRAM A10 = PPU A10)
 * ||||||+-- 1: Cartridge contains battery-backed PRG RAM ($6000-7FFF) or other persistent memory
 * |||||+--- 1: 512-byte trainer at $7000-$71FF (stored before PRG data)
 * ||||+---- 1: Alternative nametable layout
 * ++++----- Lower nybble of mapper number
 *
 * flags7
 * 7654 3210
 * ---------
 * NNNN 10TT
 * |||| ||++- Console type
 * |||| ||     0: Nintendo Entertainment System/Family Computer
 * |||| ||     1: Nintendo Vs. System
 * |||| ||     2: Nintendo Playchoice 10
 * |||| ||     3: Extended Console Type
 * |||| ++--- NES 2.0 identifier
 * ++++------ Mapper Number D7..D4
 *
 * */
impl TryFrom<&[u8]> for InesHeader {
    type Error = String;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() < 16 {
            return Err(format!(
                "iNES header requires at least 16 bytes, got {}",
                bytes.len()
            ));
        }

        if bytes[..4] != *b"NES\x1A" {
            return Err("Invalid header magic number".to_string());
        }

        let flags6 = bytes[6];
        let flags7 = bytes[7];

        if flags7 & 0x0C == 0x08 {
            return Err("NES 2.0 is not supported yet".to_string());
        }

        if flags7 & 0x0C != 0 {
            return Err("Unsupported header format".to_string());
        }

        if flags7 & 0x03 != 0 {
            return Err("Only standard NES ROMs are supported".to_string());
        }

        // 单个prg rom 16KiB, 单个chr_rom 8KiB
        let prg_rom_size = usize::from(bytes[4]) * 16 * 1024;
        let chr_rom_size = usize::from(bytes[5]) * 8 * 1024;

        let mapper_id = (flags6 >> 4) | (flags7 & 0xF0);

        let mirroring = if flags6 & 0x08 != 0 {
            Mirroring::FourScreen
        } else if flags6 & 0x01 != 0 {
            Mirroring::Vertical
        } else {
            Mirroring::Horizontal
        };
        let has_trainer = flags6 & 0x04 != 0;
        let has_battery: bool = flags6 & 0x02 != 0;

        Ok(Self {
            prg_rom_size,
            chr_rom_size,
            mapper_id,
            mirroring,
            has_trainer,
            has_battery,
        })
    }
}

#[derive(Debug)]
pub enum ChrMemory {
    Rom(Vec<u8>),
    Ram(Vec<u8>),
}

#[derive(Debug)]
pub struct Cartridge {
    header: InesHeader,
    trainer: Option<Vec<u8>>,
    prg_rom: Vec<u8>,
    chr: ChrMemory,
}

impl Cartridge {
    pub fn cpu_read(&self, addr: u16) -> Result<u8, String> {
        if addr < 0x8000 {
            return Err(format!("Address outside PRG ROM region: {addr:#06X}"));
        }

        let relative = usize::from(addr - 0x8000);

        let mask = if self.prg_rom.len() == 16 * 1024 {
            0x3FFF
        } else {
            0x7FFF
        };

        Ok(self.prg_rom[relative & mask])
    }
}

impl TryFrom<&[u8]> for Cartridge {
    type Error = String;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let header = InesHeader::try_from(bytes.get(0..16).ok_or_else(|| {
            format!(
                "cartridge header requires at least 16 bytes, got {}",
                bytes.len()
            )
        })?)?;

        if header.mapper_id != 0 {
            return Err(format!(
                "mapper_id {} has not implemented.",
                header.mapper_id
            ));
        }

        if !matches!(header.prg_rom_size, 16_384 | 32_768) {
            return Err(format!("Unsupported PRG ROM size: {}", header.prg_rom_size));
        }

        if !matches!(header.chr_rom_size, 0 | 8_192) {
            return Err(format!("Unsupported CHR ROM size: {}", header.chr_rom_size));
        }

        let mut offset = 16_usize;

        let mut trainer: Option<Vec<u8>> = None;

        if header.has_trainer {
            trainer = Some(
                bytes
                    .get(offset..offset + 512)
                    .ok_or_else(|| "fail to parse cartridge trainer".to_string())?
                    .to_vec(),
            );
            offset += 512;
        }

        let prg_rom = bytes
            .get(offset..offset + header.prg_rom_size)
            .ok_or_else(|| "fail to parse cartridge prg_rom".to_string())?
            .to_vec();

        offset += header.prg_rom_size;

        let chr = if header.chr_rom_size != 0 {
            ChrMemory::Rom(
                bytes
                    .get(offset..offset + header.chr_rom_size)
                    .ok_or_else(|| "fail to parse cartridge chr_rom".to_string())?
                    .to_vec(),
            )
        } else {
            ChrMemory::Ram(vec![0; 8192])
        };

        Ok(Self {
            header,
            trainer,
            prg_rom,
            chr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes() -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[..4].copy_from_slice(b"NES\x1A");
        bytes[4] = 2;
        bytes[5] = 1;

        bytes
    }

    #[test]
    fn parses_basic_header() {
        let bytes = header_bytes();
        let header = InesHeader::try_from(bytes.as_slice()).unwrap();

        assert_eq!(header.prg_rom_size, 32 * 1024);
        assert_eq!(header.chr_rom_size, 8 * 1024);
        assert_eq!(header.mapper_id, 0);
        assert_eq!(header.mirroring, Mirroring::Horizontal);
        assert!(!header.has_battery);
        assert!(!header.has_trainer);
    }

    #[test]
    fn parses_mapper_and_flags() {
        let mut bytes = header_bytes();
        bytes[6] = 0xB7;
        bytes[7] = 0xA0;

        let header = InesHeader::try_from(bytes.as_slice()).unwrap();

        assert_eq!(header.mapper_id, 0xAB);
        assert_eq!(header.mirroring, Mirroring::Vertical);
        assert!(header.has_battery);
        assert!(header.has_trainer);
    }

    fn make_rom(has_trainer: bool, chr_banks: u8) -> Vec<u8> {
        let mut header = [0u8; 16];
        header[..4].copy_from_slice(b"NES\x1A");
        header[4] = 1; // 16 KiB PRG
        header[5] = chr_banks;

        if has_trainer {
            header[6] |= 0x04;
        }

        let mut bytes = header.to_vec();

        if has_trainer {
            bytes.extend_from_slice(&[0xCC; 512]);
        }

        bytes.extend_from_slice(&[0xAA; 16 * 1024]);

        if chr_banks != 0 {
            bytes.extend(vec![0xBB; usize::from(chr_banks) * 8 * 1024]);
        }

        bytes
    }

    #[test]
    fn loads_trainer_prg_and_chr_without_mixing_them() {
        let bytes = make_rom(true, 1);
        let cart = Cartridge::try_from(bytes.as_slice()).unwrap();

        assert_eq!(cart.header.mapper_id, 0);
        assert_eq!(cart.trainer.as_deref(), Some(&[0xCC; 512][..]));
        assert_eq!(cart.prg_rom.as_slice(), &[0xAA; 16 * 1024]);

        match &cart.chr {
            ChrMemory::Rom(data) => {
                assert_eq!(data.as_slice(), &[0xBB; 8 * 1024]);
            }
            ChrMemory::Ram(_) => panic!("Expected CHR ROM"),
        }
    }

    #[test]
    fn allocates_chr_ram_when_chr_rom_is_absent() {
        let bytes = make_rom(false, 0);
        let cart = Cartridge::try_from(bytes.as_slice()).unwrap();

        assert!(cart.trainer.is_none());

        match &cart.chr {
            ChrMemory::Ram(data) => {
                assert_eq!(data.len(), 8 * 1024);
                assert!(data.iter().all(|&byte| byte == 0));
            }
            ChrMemory::Rom(_) => panic!("Expected CHR RAM"),
        }
    }

    #[test]
    fn rejects_every_truncated_prefix() {
        let bytes = make_rom(true, 1);

        for end in 0..bytes.len() {
            assert!(
                Cartridge::try_from(&bytes[..end]).is_err(),
                "Unexpected success for {end} bytes"
            );
        }

        assert!(Cartridge::try_from(bytes.as_slice()).is_ok());
    }

    #[test]
    fn rejects_unsupported_chr_sizes() {
        let bytes = make_rom(true, 2);

        assert!(Cartridge::try_from(bytes.as_slice()).is_err());
    }

    #[test]
    fn rejects_unsupported_prg_sizes() {
        for prg_banks in [0u8, 3] {
            let mut bytes = make_rom(false, 0);
            bytes[4] = prg_banks;

            // 无 Trainer、无 CHR ROM，调整为头部声明的完整长度。
            bytes.resize(16 + usize::from(prg_banks) * 16 * 1024, 0xAA);

            assert!(
                Cartridge::try_from(bytes.as_slice()).is_err(),
                "Should reject {prg_banks} PRG banks"
            );
        }
    }

    #[test]
    fn rejects_unsupported_mapper() {
        let mut bytes = make_rom(false, 1);
        bytes[6] = 0x10; // Mapper 1，其他标志为 0

        assert!(Cartridge::try_from(bytes.as_slice()).is_err());
    }
}
