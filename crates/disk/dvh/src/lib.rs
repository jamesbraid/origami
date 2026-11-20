//! The SGI disk volume header: the 512-byte label at block 0 of SGI disks,
//! with a directory of raw boot files and the 16-slot partition table.
//! Ported from Instigator's `dvh` and `partitions/sgi.go`.

/// Big-endian value at byte 0 of a volume header.
pub const MAGIC: u32 = 0x0be5a941;

pub const TYPE_VOLHDR: i32 = 0;
pub const TYPE_RAW: i32 = 3;
pub const TYPE_BSD: i32 = 4;
pub const TYPE_VOLUME: i32 = 6;
pub const TYPE_EFS: i32 = 7;
pub const TYPE_XFS: i32 = 10;

/// One partition slot; sizes and offsets are in 512-byte blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Partition {
    pub blocks: i32,
    pub first: i32,
    pub kind: i32,
}

impl Partition {
    pub fn start(&self) -> u64 {
        self.first as u64 * 512
    }
    pub fn len(&self) -> u64 {
        self.blocks as u64 * 512
    }
    pub fn is_empty(&self) -> bool {
        self.blocks <= 0
    }
    /// Install media give the EFS partition type 5; both are file systems.
    pub fn is_efs(&self) -> bool {
        self.kind == TYPE_EFS || self.kind == 5
    }
}

#[derive(Clone, Debug)]
pub struct BootFile {
    pub name: String,
    pub lbn: i32,
    pub bytes: i32,
}

#[derive(Clone, Debug)]
pub struct Header {
    pub boot_files: Vec<BootFile>,
    pub partitions: [Partition; 16],
    pub checksum_ok: bool,
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}

/// Parses a header from at least 512 bytes. A bad checksum is reported in
/// `checksum_ok`; it does not fail the parse.
pub fn parse(b: &[u8]) -> Result<Header, String> {
    if b.len() < 512 {
        return Err(format!("header needs 512 bytes, got {}", b.len()));
    }
    if be32(b, 0) != MAGIC {
        return Err(format!("bad magic {:#x}", be32(b, 0)));
    }
    let mut boot_files = vec![];
    for i in 0..15 {
        let off = 72 + i * 16;
        let raw = &b[off..off + 8];
        let name = &raw[..raw.iter().position(|&c| c == 0).unwrap_or(8)];
        if name.is_empty() {
            continue;
        }
        boot_files.push(BootFile {
            name: String::from_utf8_lossy(name).into(),
            lbn: be32(b, off + 8) as i32,
            bytes: be32(b, off + 12) as i32,
        });
    }
    let mut partitions = [Partition::default(); 16];
    for (i, p) in partitions.iter_mut().enumerate() {
        let off = 312 + i * 12;
        let (blocks, first) = (be32(b, off) as i32, be32(b, off + 4) as i32);
        // Negative extents are corrupt; leave the slot empty.
        if blocks < 0 || first < 0 {
            continue;
        }
        *p = Partition {
            blocks,
            first,
            kind: be32(b, off + 8) as i32,
        };
    }
    let sum = (0..128).fold(0u32, |s, i| s.wrapping_add(be32(b, i * 4)));
    Ok(Header {
        boot_files,
        partitions,
        checksum_ok: sum == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[0..4].copy_from_slice(&MAGIC.to_be_bytes());
        b[72..76].copy_from_slice(b"sash");
        b[80..84].copy_from_slice(&2u32.to_be_bytes());
        b[84..88].copy_from_slice(&1000u32.to_be_bytes());
        let p = 312 + 7 * 12;
        b[p..p + 4].copy_from_slice(&4096u32.to_be_bytes());
        b[p + 4..p + 8].copy_from_slice(&64u32.to_be_bytes());
        b[p + 8..p + 12].copy_from_slice(&7u32.to_be_bytes());
        b
    }

    #[test]
    fn parses_files_and_partitions() {
        let h = parse(&header()).unwrap();
        assert_eq!(h.boot_files[0].name, "sash");
        assert_eq!(h.boot_files[0].bytes, 1000);
        let p = h.partitions[7];
        assert!(p.is_efs() && p.start() == 64 * 512 && p.len() == 4096 * 512);
        assert!(!h.checksum_ok);
    }

    #[test]
    fn negative_extents_leave_the_slot_empty() {
        let mut b = header();
        let p = 312 + 7 * 12;
        b[p + 4..p + 8].copy_from_slice(&u32::MAX.to_be_bytes());
        let h = parse(&b).unwrap();
        assert!(h.partitions[7].is_empty());
        assert_eq!(h.partitions[7].start(), 0);
        b[p..p + 4].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        assert!(parse(&b).unwrap().partitions[7].is_empty());
    }

    #[test]
    fn checksum_balances() {
        let mut b = header();
        let sum = (0..128).fold(0u32, |s, i| s.wrapping_add(be32(&b, i * 4)));
        b[32..36].copy_from_slice(&sum.wrapping_neg().to_be_bytes());
        assert!(parse(&b).unwrap().checksum_ok);
    }

    #[test]
    fn rejects_bad_magic_and_short_input() {
        assert!(parse(&[0; 512]).is_err());
        assert!(parse(&[0; 100]).is_err());
    }
}
