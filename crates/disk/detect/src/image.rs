//! Disk image containers as positioned readers: raw, CHD and a minimal qcow2.

use chd::Chd;
use disk_rofs::{invalid, Dev, ReadAt};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Mutex;

/// Opens an image by its leading bytes: CHD, qcow2, or raw.
pub fn open(path: &Path) -> io::Result<(Dev, u64)> {
    let mut magic = [0u8; 8];
    File::open(path)?.read_exact_at(0, &mut magic)?;
    if &magic == b"MComprHD" {
        let d = ChdDev::open(path)?;
        let size = d.size;
        Ok((std::sync::Arc::new(d), size))
    } else if magic[..4] == *b"QFI\xfb" {
        let d = Qcow2::open(path)?;
        let size = d.size;
        Ok((std::sync::Arc::new(d), size))
    } else {
        let f = File::open(path)?;
        let size = f.metadata()?.len();
        Ok((std::sync::Arc::new(f), size))
    }
}

const HUNKS_CACHED: usize = 32;
const MAX_HUNK: u64 = 1 << 20;
/// Bounds the map the chd crate allocates for a header it has not checked.
const MAX_HUNKS: u64 = 1 << 23;

struct ChdState {
    chd: Chd<BufReader<File>>,
    temp: Vec<u8>,
    cache: HashMap<u32, Vec<u8>>,
}

/// A CHD v5 hard-disk image read hunk by hunk through the `chd` crate.
pub struct ChdDev {
    state: Mutex<ChdState>,
    hunk: u64,
    pub size: u64,
}

/// Checks a version 5 header before the `chd` crate allocates from it.
fn check_chd_header(file: &File) -> io::Result<()> {
    let len = file.metadata()?.len();
    let mut h = [0u8; 124];
    file.read_exact_at(0, &mut h)?;
    let be32 = |o: usize| u32::from_be_bytes(h[o..o + 4].try_into().unwrap());
    let be64 = |o: usize| u64::from_be_bytes(h[o..o + 8].try_into().unwrap());
    let (logical, hunk) = (be64(32), be32(56) as u64);
    if be32(8) != 124 || be32(12) != 5 {
        return Err(invalid("chd: not a version 5 header"));
    }
    if !(512..=MAX_HUNK).contains(&hunk) || logical == 0 || logical.div_ceil(hunk) > MAX_HUNKS {
        return Err(invalid("chd: implausible hunk or logical size"));
    }
    let map = be64(40);
    if map < 124 || map >= len {
        return Err(invalid("chd: map outside the file"));
    }
    // A compressed map starts with its byte length; an uncompressed one is 4 bytes a hunk.
    let map_len = if be32(16) != 0 {
        let mut l = [0u8; 4];
        file.read_exact_at(map, &mut l)?;
        u32::from_be_bytes(l) as u64 + 16
    } else {
        logical.div_ceil(hunk) * 4
    };
    if map + map_len > len {
        return Err(invalid("chd: map runs past the end of the file"));
    }
    Ok(())
}

/// The chd crate panics on some corrupt input; report that as an error.
fn guarded<T>(what: &str, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(invalid(format!("chd: {what} is corrupt"))))
}

impl ChdDev {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        check_chd_header(&file)?;
        guarded("header", || {
            let chd =
                Chd::open(BufReader::new(file), None).map_err(|e| invalid(format!("chd: {e}")))?;
            let (hunk, size) = (
                chd.header().hunk_size() as u64,
                chd.header().logical_bytes(),
            );
            Ok(ChdDev {
                state: Mutex::new(ChdState {
                    chd,
                    temp: vec![],
                    cache: HashMap::new(),
                }),
                hunk,
                size,
            })
        })
    }
}

impl ReadAt for ChdDev {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        if pos >= self.size {
            return Ok(0);
        }
        let n = buf
            .len()
            .min((self.size - pos) as usize)
            .min((self.hunk - pos % self.hunk) as usize);
        let idx = (pos / self.hunk) as u32;
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !st.cache.contains_key(&idx) {
            let ChdState { chd, temp, cache } = &mut *st;
            let out = guarded("hunk", || {
                let mut out = chd.get_hunksized_buffer();
                chd.hunk(idx)
                    .and_then(|mut h| h.read_hunk_in(temp, &mut out))
                    .map_err(|e| invalid(format!("chd hunk {idx}: {e}")))?;
                Ok(out)
            })?;
            if cache.len() >= HUNKS_CACHED {
                cache.clear();
            }
            cache.insert(idx, out);
        }
        let off = (pos % self.hunk) as usize;
        buf[..n].copy_from_slice(&st.cache[&idx][off..off + n]);
        Ok(n)
    }
}

const QCOW_OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;

/// A qcow2 reader for plain images: version 2 or 3, no backing file,
/// encryption, compression, external data file or extended L2 entries.
pub struct Qcow2 {
    file: File,
    cluster: u64,
    l1: Vec<u64>,
    l2: Mutex<HashMap<u64, Vec<u8>>>,
    pub size: u64,
}

impl Qcow2 {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut h = [0u8; 104];
        file.read_exact_at(0, &mut h)?;
        let be32 = |o: usize| u32::from_be_bytes(h[o..o + 4].try_into().unwrap());
        let be64 = |o: usize| u64::from_be_bytes(h[o..o + 8].try_into().unwrap());
        let version = be32(4);
        if !(2..=3).contains(&version) {
            return Err(invalid(format!("qcow2: unsupported version {version}")));
        }
        if be64(8) != 0 || be32(32) != 0 {
            return Err(invalid(
                "qcow2: backing files and encryption are not supported",
            ));
        }
        // incompatible features: corrupt (bit 1), external data (2), compression type (3), extended L2 (4)
        if version == 3 && be64(72) & 0x1e != 0 {
            return Err(invalid("qcow2: unsupported incompatible feature"));
        }
        let cluster_bits = be32(20);
        if !(9..=21).contains(&cluster_bits) {
            return Err(invalid("qcow2: implausible cluster size"));
        }
        let l1_size = be32(36) as u64;
        let flen = file.metadata()?.len();
        if be64(40).checked_add(l1_size * 8).is_none_or(|e| e > flen) {
            return Err(invalid("qcow2: L1 table runs past the end of the file"));
        }
        let mut raw = vec![0u8; l1_size as usize * 8];
        file.read_exact_at(be64(40), &mut raw)?;
        let l1 = raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_be_bytes(*c))
            .collect();
        Ok(Qcow2 {
            file,
            cluster: 1 << cluster_bits,
            l1,
            l2: Mutex::new(HashMap::new()),
            size: be64(24),
        })
    }
}

impl ReadAt for Qcow2 {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        if pos >= self.size {
            return Ok(0);
        }
        let n = buf
            .len()
            .min((self.size - pos) as usize)
            .min((self.cluster - pos % self.cluster) as usize);
        let (ci, within) = (pos / self.cluster, pos % self.cluster);
        let per_l2 = self.cluster / 8;
        let l1e = self.l1.get((ci / per_l2) as usize).copied().unwrap_or(0);
        let l2_off = l1e & QCOW_OFFSET_MASK;
        if l2_off == 0 {
            buf[..n].fill(0);
            return Ok(n);
        }
        let entry = {
            let mut cache = self.l2.lock().unwrap();
            if !cache.contains_key(&l2_off) {
                let mut t = vec![0; self.cluster as usize];
                self.file.read_exact_at(l2_off, &mut t)?;
                if cache.len() >= 64 {
                    cache.clear();
                }
                cache.insert(l2_off, t);
            }
            let i = (ci % per_l2) as usize * 8;
            u64::from_be_bytes(cache[&l2_off][i..i + 8].try_into().unwrap())
        };
        if entry >> 62 & 1 != 0 {
            return Err(invalid("qcow2: compressed clusters are not supported"));
        }
        let off = entry & QCOW_OFFSET_MASK;
        if off == 0 || entry & 1 != 0 {
            buf[..n].fill(0); // unallocated or zero cluster
        } else {
            self.file.read_exact_at(off + within, &mut buf[..n])?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 4 KiB clusters: cluster 0 header, 1 L1, 2 L2, 3 data. Virtual
    /// cluster 0 is allocated, cluster 1 is a zero cluster, 2 is unallocated.
    fn qcow2() -> Vec<u8> {
        let mut img = vec![0u8; 4 * 4096];
        img[0..4].copy_from_slice(b"QFI\xfb");
        img[4..8].copy_from_slice(&3u32.to_be_bytes());
        img[20..24].copy_from_slice(&12u32.to_be_bytes());
        img[24..32].copy_from_slice(&(3u64 * 4096).to_be_bytes());
        img[36..40].copy_from_slice(&1u32.to_be_bytes());
        img[40..48].copy_from_slice(&4096u64.to_be_bytes());
        img[4096..4104].copy_from_slice(&(2u64 * 4096).to_be_bytes());
        img[8192..8200].copy_from_slice(&(3u64 * 4096).to_be_bytes());
        img[8200..8208].copy_from_slice(&1u64.to_be_bytes());
        img[3 * 4096 + 10..3 * 4096 + 15].copy_from_slice(b"hello");
        img
    }

    fn with_file<T>(img: &[u8], f: impl FnOnce(&Path) -> T) -> T {
        let mut t = tempfile::NamedTempFile::new().unwrap();
        t.write_all(img).unwrap();
        f(t.path())
    }

    #[test]
    fn qcow2_reads_allocated_zero_and_missing_clusters() {
        with_file(&qcow2(), |p| {
            let q = Qcow2::open(p).unwrap();
            let mut b = [0xffu8; 5];
            q.read_exact_at(10, &mut b).unwrap();
            assert_eq!(&b, b"hello");
            let mut b = [0xffu8; 8192];
            q.read_exact_at(4096, &mut b).unwrap();
            assert!(b.iter().all(|&x| x == 0));
        });
    }

    #[test]
    fn qcow2_refuses_what_it_cannot_read() {
        let patch = |at: usize, v: &[u8]| {
            let mut img = qcow2();
            img[at..at + v.len()].copy_from_slice(v);
            img
        };
        let cases = [
            ("backing file", patch(8, &1u64.to_be_bytes())),
            ("encryption", patch(32, &1u32.to_be_bytes())),
            ("corrupt bit", patch(72, &2u64.to_be_bytes())),
            ("external data", patch(72, &4u64.to_be_bytes())),
            ("compression type", patch(72, &8u64.to_be_bytes())),
            ("extended L2", patch(72, &16u64.to_be_bytes())),
            ("huge L1", patch(36, &u32::MAX.to_be_bytes())),
            ("L1 offset", patch(40, &u64::MAX.to_be_bytes())),
            ("cluster bits", patch(20, &40u32.to_be_bytes())),
        ];
        for (what, img) in cases {
            assert!(with_file(&img, Qcow2::open).is_err(), "{what}");
        }
    }

    #[test]
    fn qcow2_compressed_cluster_is_an_error() {
        let mut img = qcow2();
        img[8192..8200].copy_from_slice(&((1u64 << 62) | (3 * 4096)).to_be_bytes());
        with_file(&img, |p| {
            let q = Qcow2::open(p).unwrap();
            assert!(q.read_exact_at(0, &mut [0u8; 4]).is_err());
        });
    }

    fn chd_header(patch: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut h = vec![0u8; 4096];
        h[0..8].copy_from_slice(b"MComprHD");
        h[8..12].copy_from_slice(&124u32.to_be_bytes());
        h[12..16].copy_from_slice(&5u32.to_be_bytes());
        h[32..40].copy_from_slice(&(1u64 << 20).to_be_bytes());
        h[40..48].copy_from_slice(&1024u64.to_be_bytes());
        h[56..60].copy_from_slice(&4096u32.to_be_bytes());
        patch(&mut h);
        h
    }

    #[test]
    fn hostile_chd_headers_are_refused_before_allocation() {
        type Patch = Box<dyn FnOnce(&mut Vec<u8>)>;
        let cases: [(&str, Patch); 7] = [
            (
                "logical u64::MAX",
                Box::new(|h| h[32..40].copy_from_slice(&u64::MAX.to_be_bytes())),
            ),
            ("zero hunk", Box::new(|h| h[56..60].fill(0))),
            (
                "hunk over 1 MiB",
                Box::new(|h| h[56..60].copy_from_slice(&(2u32 << 20).to_be_bytes())),
            ),
            (
                "too many hunks",
                Box::new(|h| h[32..40].copy_from_slice(&(1u64 << 40).to_be_bytes())),
            ),
            (
                "map outside file",
                Box::new(|h| h[40..48].copy_from_slice(&u64::MAX.to_be_bytes())),
            ),
            (
                "compressed map past end",
                Box::new(|h| {
                    h[16..20].copy_from_slice(b"zlib");
                    h[1024..1028].copy_from_slice(&u32::MAX.to_be_bytes());
                }),
            ),
            (
                "old version",
                Box::new(|h| h[12..16].copy_from_slice(&4u32.to_be_bytes())),
            ),
        ];
        for (what, patch) in cases {
            let img = chd_header(patch);
            assert!(with_file(&img, ChdDev::open).is_err(), "{what}");
        }
    }

    #[test]
    fn chd_panics_become_errors() {
        let r: io::Result<()> = guarded("test", || panic!("boom"));
        assert!(r.is_err());
    }
}
