//! BSD disklabel: the partition table OpenBSD and NetBSD keep in the first
//! two sectors of a disk, or of a partition on a disk with an SGI volume header.

pub const FS_BSDFFS: u8 = 7;
pub const FS_EXT2FS: u8 = 17;

const MAGIC: u32 = 0x8256_4557;
const FIXED: usize = 148;
const ENTRY: usize = 16;
const MAX_PARTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// First sector from the start of the disk, and length in sectors.
    pub offset: u64,
    pub size: u64,
    pub fstype: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub sector: u64,
    pub slots: Vec<Slot>,
}

/// Finds a label in `b`, the first 1024 bytes of a disk or partition. It
/// sits at sector 1, or sector 0 plus 64 bytes on some NetBSD ports.
pub fn find(b: &[u8]) -> Option<Label> {
    [512, 64, 0, 576].iter().find_map(|&at| parse(b.get(at..)?))
}

/// Parses the label at the start of `b`. The label is in the byte order of the
/// machine that wrote it, so the magic says which order to read.
fn parse(b: &[u8]) -> Option<Label> {
    let u32at = |o: usize, big: bool| {
        let v = b.get(o..o + 4)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(v)
        } else {
            u32::from_le_bytes(v)
        })
    };
    let big = if u32at(0, true)? == MAGIC {
        true
    } else if u32at(0, false)? == MAGIC {
        false
    } else {
        return None;
    };
    let half = |o: usize| {
        let v = b.get(o..o + 2)?.try_into().ok()?;
        Some(if big {
            u16::from_be_bytes(v)
        } else {
            u16::from_le_bytes(v)
        })
    };
    let n = half(138)? as usize;
    if u32at(132, big)? != MAGIC || n > MAX_PARTS {
        return None;
    }
    let end = FIXED + n * ENTRY;
    // The words of a good label XOR to zero in either byte order.
    let sum = (b.get(..end)?.as_chunks::<2>().0.iter()).fold(0u16, |x, w| x ^ half_of(w, big));
    if sum != 0 {
        return None;
    }
    let sector = match u32at(40, big)? {
        s if s.is_power_of_two() && (512..=65536).contains(&s) => s as u64,
        _ => 512,
    };
    // OpenBSD (d_version 1) keeps the high halves of offset and size at bytes 8 and 10
    // of a slot; NetBSD keeps a fragment size there.
    let openbsd = half(114)? == 1;
    let slots = (0..n)
        .filter_map(|i| {
            let o = FIXED + i * ENTRY;
            let (mut size, mut offset) = (u32at(o, big)? as u64, u32at(o + 4, big)? as u64);
            if openbsd {
                offset |= (half(o + 8)? as u64) << 32;
                size |= (half(o + 10)? as u64) << 32;
            }
            Some(Slot {
                offset,
                size,
                fstype: *b.get(o + 12)?,
            })
        })
        .collect();
    Some(Label { sector, slots })
}

fn half_of(w: &[u8; 2], big: bool) -> u16 {
    if big {
        u16::from_be_bytes(*w)
    } else {
        u16::from_le_bytes(*w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(big: bool, at: usize, slots: &[(u32, u32, u8)], highs: bool) -> Vec<u8> {
        let mut b = vec![0u8; 1024];
        let put = |b: &mut Vec<u8>, o: usize, v: u32| {
            let v = if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            };
            b[at + o..at + o + 4].copy_from_slice(&v);
        };
        put(&mut b, 0, MAGIC);
        put(&mut b, 40, 512);
        put(&mut b, 132, MAGIC);
        let n = if big {
            (slots.len() as u16).to_be_bytes()
        } else {
            (slots.len() as u16).to_le_bytes()
        };
        b[at + 138..at + 140].copy_from_slice(&n);
        for (i, &(size, off, ty)) in slots.iter().enumerate() {
            let o = FIXED + i * ENTRY;
            put(&mut b, o, size);
            put(&mut b, o + 4, off);
            b[at + o + 12] = ty;
        }
        if highs {
            let half = |v: u16| {
                if big {
                    v.to_be_bytes()
                } else {
                    v.to_le_bytes()
                }
            };
            b[at + 114..at + 116].copy_from_slice(&half(1)); // d_version
            b[at + FIXED + 8..at + FIXED + 10].copy_from_slice(&half(1)); // offseth
            b[at + FIXED + 10..at + FIXED + 12].copy_from_slice(&half(2)); // sizeh
        }
        let x = (b[at..at + FIXED + slots.len() * ENTRY]
            .as_chunks::<2>()
            .0
            .iter())
        .fold(0u16, |x, w| x ^ half_of(w, big));
        let x = if big {
            x.to_be_bytes()
        } else {
            x.to_le_bytes()
        };
        b[at + 136..at + 138].copy_from_slice(&x);
        b
    }

    #[test]
    fn reads_either_order_at_either_place() {
        for (big, at) in [(true, 512), (false, 512), (false, 64), (true, 0)] {
            let l = find(&label(big, at, &[(1000, 64, 7), (50, 0, 0)], false)).unwrap();
            assert_eq!(l.sector, 512);
            assert_eq!(
                l.slots[0],
                Slot {
                    offset: 64,
                    size: 1000,
                    fstype: 7
                }
            );
            assert_eq!(l.slots.len(), 2);
        }
    }

    #[test]
    fn openbsd_high_halves_are_read_only_for_openbsd_labels() {
        let slot = |highs| {
            find(&label(false, 512, &[(1000, 64, 7)], highs))
                .unwrap()
                .slots[0]
        };
        assert_eq!(slot(true).offset, 64 | 1 << 32);
        assert_eq!(slot(true).size, 1000 | 2 << 32);
        assert_eq!(slot(false).size, 1000);
    }

    #[test]
    fn refuses_what_does_not_hold_together() {
        let good = label(true, 512, &[(1000, 64, 7)], false);
        let mut bad = good.clone();
        bad[512 + FIXED + 5] ^= 1; // checksum
        assert!(find(&bad).is_none());
        let mut bad = good.clone();
        bad[512 + 139] = 200; // 200 partitions
        assert!(find(&bad).is_none());
        let mut bad = good;
        bad[512 + 135] ^= 1; // second magic
        assert!(find(&bad).is_none());
        assert!(find(&[0; 1024]).is_none());
        assert!(find(&[0; 10]).is_none());
    }
}
