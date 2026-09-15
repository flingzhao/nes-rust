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
}
