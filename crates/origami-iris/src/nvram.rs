//! The Indy's NVRAM image: the DS1386's 8 KiB, holding the PROM's 256-byte
//! settings block after 64 bytes of clock registers.
//!
//! A new machine's image is blank, and iris would fill it with the PROM's
//! defaults at the first start. [`seed`] writes them first, with the console
//! the caller picks: `g` where the machine has a graphics head, `d` without one.

/// The Ethernet address of a machine with no identity: iris's own default,
/// the first of its emulator-minted block.
pub const DEFAULT_MAC: &str = "08:00:69:e2:00:00";

/// An Octane's DS1685 image.
pub const OCTANE_SIZE: u64 = 512;

/// The Indigo2's 93C56 EEPROM.
const EEPROM_SIZE: u64 = 256;

/// The DS1386's size, which iris saves the whole of.
pub const SIZE: u64 = 8192;

use std::path::Path;

/// Where the settings block starts in the image. Its last six bytes are the
/// Ethernet address.
const BLOCK: usize = 64;
/// The layout revision the Indy PROM writes.
const REV: u8 = 8;

/// The block's checksum: seeded with 0xA5, every byte but its own XORed in,
/// rotating left one bit after each 16-bit word.
fn checksum(block: &[u8; 256]) -> u8 {
    let mut sum = 0xA5u8;
    for (i, pair) in block.as_chunks::<2>().0.iter().enumerate() {
        if i != 0 {
            sum ^= pair[0];
        }
        sum ^= pair[1];
        sum = sum.rotate_left(1);
    }
    sum
}

/// The settings block of a new Indy: the PROM's factory settings with `console`
/// (`b'g'` or `b'd'`). The checksum covers the Ethernet address, which
/// iris injects at every start only while the slot is blank, so it goes in
/// here.
fn block(mac: [u8; 6], console: u8) -> [u8; 256] {
    let mut block = [0u8; 256];
    block[1] = REV;
    block[2] = console;
    for (offset, value) in [
        (116, "9600"),
        (121, "0"),
        (122, "PST8PDT"),
        (178, "Y"),
        (232, "80"),
        (236, "y"),
        (239, "y"),
    ] {
        block[offset..offset + value.len()].copy_from_slice(value.as_bytes());
    }
    block[250..].copy_from_slice(&mac);
    block[0] = checksum(&block);
    block
}

/// The six bytes of `colon:separated:hex`.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut out = [0u8; 6];
    let mut parts = text.split(':');
    for byte in &mut out {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Which store a settings block goes in.
#[derive(Clone, Copy)]
pub enum Kind {
    /// The Indy's battery RAM; blank is zero.
    Indy,
    /// The Indigo2's EEPROM, which is the block itself; blank is 0xFF.
    Indigo2,
}

impl Kind {
    pub fn of(initial: &str) -> Option<Kind> {
        match initial {
            "indy-nvram" => Some(Kind::Indy),
            "indigo2-eeprom" => Some(Kind::Indigo2),
            _ => None,
        }
    }

    /// A new store: blank, so iris and the PROM treat it as never started.
    pub fn blank(self) -> Vec<u8> {
        match self {
            Kind::Indy => vec![0; SIZE as usize],
            Kind::Indigo2 => vec![0xFF; EEPROM_SIZE as usize],
        }
    }

    fn at(self) -> (usize, u8, u64) {
        match self {
            Kind::Indy => (BLOCK, 0, SIZE),
            Kind::Indigo2 => (0, 0xFF, EEPROM_SIZE),
        }
    }
}

/// Write the factory settings into the image at `path` if the machine has
/// never started: its settings block is still blank.
pub fn seed(path: &Path, kind: Kind, mac: [u8; 6], console: u8) -> std::io::Result<()> {
    let (at, blank, size) = kind.at();
    let mut image = std::fs::read(path)?;
    if image.len() as u64 != size || image[at..at + 250].iter().any(|&b| b != blank) {
        return Ok(());
    }
    image[at..at + 256].copy_from_slice(&block(mac, console));
    std::fs::write(path, image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_image_is_seeded_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nvram.raw");
        std::fs::write(&path, vec![0u8; SIZE as usize]).unwrap();
        seed(&path, Kind::Indy, [8, 0, 0x69, 1, 2, 3], b'g').unwrap();
        let image = std::fs::read(&path).unwrap();
        let seeded: [u8; 256] = image[BLOCK..BLOCK + 256].try_into().unwrap();
        assert_eq!(seeded[2], b'g');
        assert_eq!(&seeded[250..], &[8, 0, 0x69, 1, 2, 3]);
        assert_eq!(seeded[0], checksum(&seeded));
        // The Indigo2's EEPROM is blank at 0xFF and holds the block from byte 0.
        std::fs::write(&path, Kind::Indigo2.blank()).unwrap();
        seed(&path, Kind::Indigo2, [8, 0, 0x69, 1, 2, 3], b'g').unwrap();
        assert_eq!(std::fs::read(&path).unwrap()[..256], seeded);
        std::fs::write(&path, Kind::Indy.blank()).unwrap();
        seed(&path, Kind::Indy, [8, 0, 0x69, 1, 2, 3], b'g').unwrap();
        // A guest's own settings are never overwritten.
        seed(&path, Kind::Indy, [8, 0, 0x69, 9, 9, 9], b'd').unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), image);
        // Without a graphics head the console is the serial line.
        std::fs::write(&path, Kind::Indy.blank()).unwrap();
        seed(&path, Kind::Indy, [8, 0, 0x69, 1, 2, 3], b'd').unwrap();
        assert_eq!(std::fs::read(&path).unwrap()[BLOCK + 2], b'd');
    }
}
