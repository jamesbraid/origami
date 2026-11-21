//! Read-only BSD fast file system: UFS1 (128-byte inodes, 32-bit block
//! pointers) and UFS2 (256-byte inodes, 64-bit pointers), in either byte
//! order. Ported from Instigator's `ffs` package, which reads UFS1 only.

use disk_rofs::{invalid, not_found, DirEntry, FileType, ReadAt, ReadFs, Stat};
use std::io;

const MAGIC_UFS1: u32 = 0x011954;
const MAGIC_UFS2: u32 = 0x19540119;
/// Where a superblock can sit: UFS2, UFS1, a copy at 0, and the "piggy" spot.
const SB_OFFSETS: [u64; 4] = [65536, 8192, 0, 262144];
const SB_MAGIC: usize = 1372;
const SB_LEN: usize = SB_MAGIC + 4;
const ROOT_INO: u32 = 2;
const NDADDR: usize = 12;
const NIADDR: usize = 3;
/// A corrupt size must not send a reader over terabytes of holes.
const MAX_FILE: u64 = 1 << 44;
const MAX_DIR: u64 = 64 << 20;
/// Records examined in one lookup.
const MAX_SCAN: u32 = 1 << 18;
const MAX_LINK: u64 = 4096;
const DIR_CHUNK: usize = 512;
const DIR_HDR: usize = 8;
const READ_STEP: usize = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Big,
    Little,
}

impl Order {
    fn u16(self, b: &[u8], o: usize) -> Option<u16> {
        let v = b.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(match self {
            Order::Big => u16::from_be_bytes(v),
            Order::Little => u16::from_le_bytes(v),
        })
    }
    fn u32(self, b: &[u8], o: usize) -> Option<u32> {
        let v = b.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(match self {
            Order::Big => u32::from_be_bytes(v),
            Order::Little => u32::from_le_bytes(v),
        })
    }
    fn u64(self, b: &[u8], o: usize) -> Option<u64> {
        let v = b.get(o..o.checked_add(8)?)?.try_into().ok()?;
        Some(match self {
            Order::Big => u64::from_be_bytes(v),
            Order::Little => u64::from_le_bytes(v),
        })
    }
}

fn short() -> io::Error {
    invalid("ffs: short structure")
}

/// Superblock geometry, validated.
struct Geo {
    ord: Order,
    ufs2: bool,
    bsize: u64,
    fsize: u64,
    frag: u64,
    fs_frags: u64,
    iblkno: u64,
    cgoff: u64,
    cgmask: u32,
    fpg: u64,
    ipg: u64,
    ncg: u64,
    inopb: u64,
    nindir: u64,
    max_symlink: i32,
}

fn pow2(v: u64, lo: u64, hi: u64) -> bool {
    (lo..=hi).contains(&v) && v & (v - 1) == 0
}

impl Geo {
    fn read(dev: &dyn ReadAt, at: u64) -> io::Result<Geo> {
        let mut sb = [0u8; SB_LEN];
        dev.read_exact_at(at, &mut sb)?;
        let be = u32::from_be_bytes(sb[SB_MAGIC..].try_into().unwrap());
        let le = u32::from_le_bytes(sb[SB_MAGIC..].try_into().unwrap());
        let (ord, magic) = if be == MAGIC_UFS1 || be == MAGIC_UFS2 {
            (Order::Big, be)
        } else if le == MAGIC_UFS1 || le == MAGIC_UFS2 {
            (Order::Little, le)
        } else {
            return Err(invalid(format!("ffs: bad superblock magic {be:#x}")));
        };
        let ufs2 = magic == MAGIC_UFS2;
        let i32f = |o: usize| ord.u32(&sb, o).map(|v| v as i32 as i64).ok_or_else(short);
        let pos = |v: i64| u64::try_from(v).map_err(|_| invalid("ffs: negative geometry"));
        let fs_frags = if ufs2 {
            pos(ord.u64(&sb, 1080).ok_or_else(short)? as i64)?
        } else {
            pos(i32f(36)?)?
        };
        let g = Geo {
            ord,
            ufs2,
            iblkno: pos(i32f(16)?)?,
            cgoff: pos(i32f(24)?)?,
            cgmask: ord.u32(&sb, 28).ok_or_else(short)?,
            ncg: pos(i32f(44)?)?,
            bsize: pos(i32f(48)?)?,
            fsize: pos(i32f(52)?)?,
            frag: pos(i32f(56)?)?,
            inopb: pos(i32f(120)?)?,
            ipg: pos(i32f(184)?)?,
            fpg: pos(i32f(188)?)?,
            fs_frags,
            nindir: 0,
            max_symlink: i32f(1320)? as i32,
        };
        let isize = g.inode_size();
        if !pow2(g.bsize, 512, 65536) || !pow2(g.fsize, 512, g.bsize) || g.frag != g.bsize / g.fsize
        {
            return Err(invalid(format!(
                "ffs: implausible block size {} / fragment size {}",
                g.bsize, g.fsize
            )));
        }
        if g.inopb != g.bsize / isize {
            return Err(invalid("ffs: inodes per block do not match the inode size"));
        }
        if g.ncg == 0
            || g.ipg == 0
            || g.fpg == 0
            || g.fs_frags == 0
            || g.iblkno == 0
            || g.iblkno >= g.fpg
            || g.ncg.checked_mul(g.ipg).is_none_or(|n| n > u32::MAX as u64)
            || g.fs_frags
                .checked_mul(g.fsize)
                .is_none_or(|n| n > i64::MAX as u64)
        {
            return Err(invalid("ffs: implausible geometry"));
        }
        Ok(Geo {
            nindir: g.bsize / g.ptr_size(),
            ..g
        })
    }

    fn inode_size(&self) -> u64 {
        if self.ufs2 {
            256
        } else {
            128
        }
    }

    fn ptr_size(&self) -> u64 {
        if self.ufs2 {
            8
        } else {
            4
        }
    }

    fn ptr(&self, b: &[u8], i: usize) -> io::Result<u64> {
        let r = if self.ufs2 {
            self.ord.u64(b, i * 8)
        } else {
            self.ord.u32(b, i * 4).map(u64::from)
        };
        r.ok_or_else(short)
    }
}

struct Inode {
    mode: u16,
    size: u64,
    blocks: u64,
    /// The twelve direct then three indirect pointers, as stored.
    area: Vec<u8>,
}

impl Inode {
    fn file_type(&self) -> FileType {
        match self.mode & 0o170000 {
            0o100000 => FileType::File,
            0o040000 => FileType::Dir,
            0o120000 => FileType::Symlink,
            _ => FileType::Other,
        }
    }
}

pub struct Ffs<D> {
    dev: D,
    g: Geo,
}

impl<D: ReadAt + Send + Sync> Ffs<D> {
    /// Opens the file system at the start of `dev`, whichever superblock
    /// location holds a usable one.
    pub fn open(dev: D) -> io::Result<Self> {
        let mut last = invalid("ffs: no superblock");
        let mut dev = dev;
        for at in SB_OFFSETS {
            match Geo::read(&dev, at) {
                Ok(g) => {
                    let fs = Ffs { dev, g };
                    match fs.inode(ROOT_INO) {
                        Ok(r) if r.file_type() == FileType::Dir => return Ok(fs),
                        Ok(_) => last = invalid("ffs: root inode is not a directory"),
                        Err(e) => last = e,
                    }
                    dev = fs.dev;
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Reads `n` bytes at fragment address `daddr`; an address outside the
    /// file system is corruption whatever the device holds there.
    fn read_frags(&self, daddr: u64, n: u64) -> io::Result<Vec<u8>> {
        let g = &self.g;
        if daddr == 0 || daddr >= g.fs_frags || n.div_ceil(g.fsize) > g.fs_frags - daddr {
            return Err(invalid(format!(
                "ffs: block address {daddr} outside the file system"
            )));
        }
        let mut b = vec![0; n as usize];
        self.dev.read_exact_at(daddr * g.fsize, &mut b)?;
        Ok(b)
    }

    fn inode(&self, n: u32) -> io::Result<Inode> {
        let g = &self.g;
        let n = n as u64;
        if n < 1 || n >= g.ncg * g.ipg {
            return Err(invalid(format!("ffs: inode {n} out of range")));
        }
        let (cg, idx) = (n / g.ipg, n % g.ipg);
        let start = if g.ufs2 {
            g.fpg.saturating_mul(cg)
        } else {
            // Groups are staggered by cgoffset times the group number's low bits.
            let stagger = g.cgoff.saturating_mul(cg & !(g.cgmask as u64));
            g.fpg.saturating_mul(cg).saturating_add(stagger)
        };
        let daddr = start
            .saturating_add(g.iblkno)
            .saturating_add(idx / g.inopb * g.frag);
        let blk = self.read_frags(daddr, g.bsize)?;
        let d = blk
            .get((idx % g.inopb * g.inode_size()) as usize..)
            .and_then(|d| d.get(..g.inode_size() as usize))
            .ok_or_else(short)?;
        let o = g.ord;
        let mode = o.u16(d, 0).ok_or_else(short)?;
        let (size, blocks, area) = if g.ufs2 {
            (
                o.u64(d, 16).ok_or_else(short)?,
                o.u64(d, 24).ok_or_else(short)?,
                d[112..232].to_vec(),
            )
        } else {
            let size = o.u64(d, 8).ok_or_else(short)?;
            (
                size,
                o.u32(d, 104).ok_or_else(short)? as u64,
                d[40..100].to_vec(),
            )
        };
        if size > MAX_FILE {
            return Err(invalid(format!("ffs: inode {n} size {size}")));
        }
        Ok(Inode {
            mode,
            size,
            blocks,
            area,
        })
    }

    /// The fragment address of logical block `lbn`, or 0 for a hole.
    fn bmap(&self, ino: &Inode, mut lbn: u64) -> io::Result<u64> {
        let g = &self.g;
        if lbn < NDADDR as u64 {
            return g.ptr(&ino.area, lbn as usize);
        }
        lbn -= NDADDR as u64;
        let mut span = 1u64;
        for lvl in 0..NIADDR {
            span *= g.nindir;
            if lbn >= span {
                lbn -= span;
                continue;
            }
            let mut p = g.ptr(&ino.area, NDADDR + lvl)?;
            for _ in 0..=lvl {
                if p == 0 {
                    break;
                }
                let blk = self.read_frags(p, g.bsize)?;
                span /= g.nindir;
                p = g.ptr(&blk, (lbn / span) as usize)?;
                lbn %= span;
            }
            return Ok(p);
        }
        Err(invalid("ffs: block beyond the triple indirect range"))
    }

    fn read_inode_at(&self, ino: &Inode, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        let g = &self.g;
        if pos >= ino.size {
            return Ok(0);
        }
        let want = buf
            .len()
            .min((ino.size - pos).min(usize::MAX as u64) as usize);
        let mut done = 0;
        while done < want {
            let at = pos + done as u64;
            let (lbn, within) = (at / g.bsize, at % g.bsize);
            let n = (g.bsize - within).min((want - done) as u64) as usize;
            let daddr = self.bmap(ino, lbn)?;
            if daddr == 0 {
                buf[done..done + n].fill(0); // hole
            } else {
                // Only the bytes asked for are read: a file's tail lives in
                // fragments and the rest of its block belongs to someone else.
                let end = daddr
                    .checked_mul(g.fsize)
                    .and_then(|b| b.checked_add(within + n as u64));
                if daddr >= g.fs_frags || end.is_none_or(|e| e > g.fs_frags * g.fsize) {
                    return Err(invalid(format!(
                        "ffs: block at address {daddr} outside the file system"
                    )));
                }
                self.dev
                    .read_exact_at(daddr * g.fsize + within, &mut buf[done..done + n])?;
            }
            done += n;
        }
        Ok(done)
    }

    /// Up to `cap` bytes of a file, grown as data arrives so a hostile size
    /// reserves nothing up front.
    fn read_all(&self, ino: &Inode, cap: u64) -> io::Result<Vec<u8>> {
        let size = ino.size.min(cap);
        let mut buf = Vec::new();
        while (buf.len() as u64) < size {
            let at = buf.len();
            let m = (size - at as u64).min(READ_STEP as u64) as usize;
            buf.resize(at + m, 0);
            match self.read_inode_at(ino, at as u64, &mut buf[at..]) {
                Ok(n) => {
                    buf.truncate(at + n);
                    if n < m {
                        break;
                    }
                }
                Err(e) if at == 0 => return Err(e),
                Err(_) => {
                    buf.truncate(at);
                    break;
                }
            }
        }
        Ok(buf)
    }

    /// Streams `dir` and stops at the first record named `name`, so a huge
    /// directory costs one block of memory and a bounded scan.
    fn find_entry(&self, dir: &Inode, name: &str) -> io::Result<Option<u32>> {
        if dir.file_type() != FileType::Dir {
            return Err(invalid("ffs: not a directory"));
        }
        let g = &self.g;
        let size = dir.size.min(MAX_DIR);
        let mut buf = vec![0u8; g.bsize as usize];
        let (mut pos, mut scanned) = (0, 0);
        while pos < size {
            let n = (size - pos).min(g.bsize) as usize;
            if !matches!(self.read_inode_at(dir, pos, &mut buf[..n]), Ok(m) if m == n) {
                return Ok(None);
            }
            for chunk in buf[..n].chunks(DIR_CHUNK) {
                let mut p = 0;
                while p + DIR_HDR <= chunk.len() {
                    scanned += 1;
                    let ino = g.ord.u32(chunk, p).unwrap_or(0);
                    let reclen = g.ord.u16(chunk, p + 4).unwrap_or(0) as usize;
                    // 4.4BSD has a type byte then a length byte; 4.3BSD a 16-bit length.
                    let namlen = match g.ord {
                        Order::Big => chunk[p + 7],
                        Order::Little if chunk[p + 7] != 0 => chunk[p + 7],
                        Order::Little => chunk[p + 6],
                    } as usize;
                    if scanned > MAX_SCAN
                        || reclen < DIR_HDR
                        || !reclen.is_multiple_of(4)
                        || p + reclen > chunk.len()
                        || DIR_HDR + namlen > reclen
                    {
                        break; // the rest of a damaged chunk is lost, not the others
                    }
                    let found = &chunk[p + DIR_HDR..p + DIR_HDR + namlen];
                    p += reclen;
                    if ino != 0 && (ino as u64) < g.ncg * g.ipg && found == name.as_bytes() {
                        return Ok(Some(ino));
                    }
                }
                if scanned > MAX_SCAN {
                    return Ok(None);
                }
            }
            pos += n as u64;
        }
        Ok(None)
    }

    fn lookup(&self, path: &str) -> io::Result<Inode> {
        let mut ino = self.inode(ROOT_INO)?;
        let parts: Vec<_> = path
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        for (i, part) in parts.iter().enumerate() {
            let next = self
                .find_entry(&ino, part)
                .map_err(|_| not_found(path))?
                .ok_or_else(|| not_found(path))?;
            ino = self.inode(next)?;
            if ino.file_type() == FileType::Symlink && i + 1 < parts.len() {
                return Err(invalid(format!("{path}: symlink in path")));
            }
        }
        Ok(ino)
    }
}

struct File<'a, D> {
    fs: &'a Ffs<D>,
    ino: Inode,
}

impl<D: ReadAt + Send + Sync> ReadAt for File<'_, D> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.fs.read_inode_at(&self.ino, pos, buf)
    }
}

impl<D: ReadAt + Send + Sync> ReadFs for Ffs<D> {
    fn stat(&self, path: &str) -> io::Result<Stat> {
        let i = self.lookup(path)?;
        Ok(Stat {
            file_type: i.file_type(),
            size: i.size,
        })
    }

    fn open(&self, path: &str) -> io::Result<Box<dyn ReadAt + '_>> {
        Ok(Box::new(File {
            fs: self,
            ino: self.lookup(path)?,
        }))
    }

    /// Detection only looks files up by path, so listings are not offered.
    fn read_dir(&self, _: &str) -> io::Result<Vec<DirEntry>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ffs: no directory listing",
        ))
    }

    fn read_link(&self, path: &str) -> io::Result<String> {
        let i = self.lookup(path)?;
        if i.file_type() != FileType::Symlink {
            return Err(invalid("ffs: not a symlink"));
        }
        if i.size > MAX_LINK {
            return Err(invalid("ffs: symlink target too long"));
        }
        // 4.4BSD keeps short targets in the pointer area, as the superblock's
        // limit says; older volumes mark them by holding no blocks.
        let g = &self.g;
        let inline = if g.max_symlink > 0 {
            i.size < g.max_symlink as u64
        } else {
            i.blocks == 0
        };
        let bytes = if inline {
            i.area
                .get(..i.size as usize)
                .map(<[u8]>::to_vec)
                .ok_or_else(short)?
        } else {
            self.read_all(&i, MAX_LINK)?
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FSIZE: usize = 1024;
    const BSIZE: usize = 8192;
    const INODES: usize = 80 * FSIZE;

    struct Img {
        b: Vec<u8>,
        ufs2: bool,
        big: bool,
    }

    impl Img {
        fn put(&mut self, o: usize, v: u64, n: usize) {
            let bytes = if self.big {
                v.to_be_bytes()[8 - n..].to_vec()
            } else {
                v.to_le_bytes()[..n].to_vec()
            };
            self.b[o..o + n].copy_from_slice(&bytes);
        }

        /// One cylinder group of 512 fragments; inodes at fragment 80.
        fn new(ufs2: bool, big: bool) -> Img {
            let mut i = Img {
                b: vec![0; 512 * FSIZE],
                ufs2,
                big,
            };
            let sb = if ufs2 { 65536 } else { 8192 };
            let inopb = BSIZE / if ufs2 { 256 } else { 128 };
            for (o, v) in [
                (16, 80),
                (28, 0xffff_ffff),
                (44, 1),
                (48, BSIZE),
                (52, FSIZE),
                (56, 8),
                (120, inopb),
                (184, inopb),
                (188, 512),
                (1320, if ufs2 { 120 } else { 60 }),
            ] {
                i.put(sb + o, v as u64, 4);
            }
            i.put(
                sb + if ufs2 { 1080 } else { 36 },
                512,
                if ufs2 { 8 } else { 4 },
            );
            i.put(
                sb + 1372,
                if ufs2 { MAGIC_UFS2 } else { MAGIC_UFS1 } as u64,
                4,
            );
            i
        }

        fn inode(&mut self, n: usize, mode: u16, size: u64, ptrs: &[u64]) {
            let (isz, area, szo) = if self.ufs2 {
                (256, 112, 16)
            } else {
                (128, 40, 8)
            };
            let o = INODES + n * isz;
            self.put(o, mode as u64, 2);
            self.put(o + szo, size, 8);
            for (k, &p) in ptrs.iter().enumerate() {
                let (po, pw) = if self.ufs2 {
                    (area + k * 8, 8)
                } else {
                    (area + k * 4, 4)
                };
                self.put(o + po, p, pw);
            }
        }

        fn dir(&mut self, frag: usize, ents: &[(&str, u32)]) {
            let base = frag * FSIZE;
            let mut p = 0;
            for (k, (name, ino)) in ents.iter().enumerate() {
                let mut len = (DIR_HDR + name.len() + 1).next_multiple_of(4);
                if k + 1 == ents.len() {
                    len = DIR_CHUNK - p;
                }
                self.put(base + p, *ino as u64, 4);
                self.put(base + p + 4, len as u64, 2);
                self.b[base + p + 6] = 4;
                self.b[base + p + 7] = name.len() as u8;
                self.b[base + p + 8..base + p + 8 + name.len()].copy_from_slice(name.as_bytes());
                p += len;
            }
        }

        /// /hello, /lnk -> abc, /d/x, and /big of 13 blocks (the last via the indirect block).
        fn populated(ufs2: bool, big: bool) -> Img {
            let mut i = Img::new(ufs2, big);
            i.inode(2, 0o40755, 512, &[88]);
            i.dir(
                88,
                &[
                    (".", 2),
                    ("..", 2),
                    ("hello", 3),
                    ("lnk", 4),
                    ("big", 5),
                    ("d", 6),
                ],
            );
            i.inode(3, 0o100644, 5, &[96]);
            i.b[96 * FSIZE..96 * FSIZE + 5].copy_from_slice(b"world");
            i.inode(4, 0o120777, 3, &[]);
            let link = INODES + 4 * if ufs2 { 256 } else { 128 } + if ufs2 { 112 } else { 40 };
            i.b[link..link + 3].copy_from_slice(b"abc");
            if !ufs2 {
                // a 4.3BSD-style volume marks inline links by holding no blocks
                i.put(8192 + 1320, 0, 4);
            }
            let mut ptrs: Vec<u64> = (0..12).map(|k| 104 + 8 * k).collect();
            ptrs.extend([0, 0, 0]);
            ptrs[12] = 200;
            i.inode(5, 0o100644, 13 * BSIZE as u64, &ptrs);
            for k in 0..12 {
                i.b[(104 + 8 * k) * FSIZE..][..BSIZE].fill(k as u8 + 1);
            }
            let pw = if ufs2 { 8 } else { 4 };
            i.put(200 * FSIZE, 208, pw);
            i.b[208 * FSIZE..][..BSIZE].fill(13);
            i.inode(6, 0o40755, 512, &[216]);
            i.dir(216, &[(".", 6), ("..", 2), ("x", 3)]);
            i
        }
    }

    fn check(ufs2: bool, big: bool) {
        let mut img = Img::populated(ufs2, big);
        if ufs2 {
            // blocks held is in bytes there; the target length decides
            img.put(INODES + 4 * 256 + 24, 0, 8);
        }
        let fs = Ffs::open(img.b).unwrap();
        for (path, ty) in [
            ("big", FileType::File),
            ("d", FileType::Dir),
            ("hello", FileType::File),
            ("lnk", FileType::Symlink),
        ] {
            assert_eq!(fs.stat(path).unwrap().file_type, ty, "{path}");
        }
        assert!(fs.read_dir(".").is_err());
        let mut b = [0; 5];
        fs.open("hello").unwrap().read_exact_at(0, &mut b).unwrap();
        assert_eq!(&b, b"world");
        assert_eq!(fs.stat("d/x").unwrap().size, 5);
        assert_eq!(fs.read_link("lnk").unwrap(), "abc");
        assert!(fs.stat("lnk/x").is_err());
        let big_file = fs.open("big").unwrap();
        for k in [0u64, 11, 12] {
            let mut b = [0; 4];
            big_file
                .read_exact_at(k * BSIZE as u64 + 100, &mut b)
                .unwrap();
            assert_eq!(b, [k as u8 + 1; 4], "block {k}");
        }
        assert!(fs.stat("missing").is_err());
    }

    #[test]
    fn ufs1_big_endian() {
        check(false, true);
    }

    #[test]
    fn ufs1_little_endian() {
        check(false, false);
    }

    #[test]
    fn ufs2_little_endian() {
        check(true, false);
    }

    #[test]
    fn ufs2_big_endian() {
        check(true, true);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Ffs::open(vec![0u8; 1 << 20]).is_err());
        assert!(Ffs::open(vec![0u8; 100]).is_err());
    }

    #[test]
    fn hostile_geometry_and_pointers_are_errors() {
        let sb = 8192;
        let patch = |at: usize, v: u32| {
            let mut i = Img::populated(false, true);
            i.put(sb + at, v as u64, 4);
            i.b
        };
        for (what, img) in [
            ("block size", patch(48, 3000)),
            ("fragment size", patch(52, 16384)),
            ("no groups", patch(44, 0)),
            ("negative size", patch(36, 0x8000_0000)),
            ("inode block outside", patch(16, 0x7fff_ffff)),
        ] {
            assert!(Ffs::open(img).is_err(), "{what}");
        }
        let mut i = Img::populated(false, true);
        i.put(sb + 184, 0x7fff_ffff, 4);
        i.put(sb + 44, 4, 4);
        assert!(Ffs::open(i.b).is_err(), "inode count over 32 bits");
        // a data pointer far outside the volume, and a hostile size
        let mut i = Img::populated(false, true);
        i.put(INODES + 3 * 128 + 40, 0xffff_ff00, 4);
        let fs = Ffs::open(i.b.clone()).unwrap();
        assert!(fs
            .open("hello")
            .unwrap()
            .read_exact_at(0, &mut [0; 5])
            .is_err());
        i.put(INODES + 3 * 128 + 8, u64::MAX, 8);
        let fs = Ffs::open(i.b).unwrap();
        assert!(fs.stat("hello").is_err());
    }

    #[test]
    fn damaged_directory_chunk_does_not_loop_or_hide_the_rest() {
        let mut i = Img::populated(false, true);
        i.put(88 * FSIZE + 4, 0, 2); // "." now has a record length of zero
        let fs = Ffs::open(i.b).unwrap();
        assert!(fs.stat("hello").is_err());
    }

    #[test]
    fn open_falls_back_to_the_next_superblock_when_the_root_is_unreadable() {
        let mut i = Img::populated(false, true);
        let copy = i.b[8192..8192 + SB_LEN].to_vec();
        i.b[65536..65536 + SB_LEN].copy_from_slice(&copy);
        i.put(65536 + 16, 500, 4); // inodes would be past the file system
        assert!(Ffs::open(i.b).unwrap().stat("hello").is_ok());
    }

    #[test]
    fn huge_directory_is_scanned_in_bounded_memory() {
        let mut i = Img::populated(false, true);
        i.put(INODES + 2 * 128 + 8, 60 << 20, 8); // a root of 60 MiB of holes
        let fs = Ffs::open(i.b).unwrap();
        assert!(fs.stat("hello").is_ok());
        assert!(fs.stat("missing").is_err());
    }

    /// Flips random bytes through the metadata of a populated volume and
    /// walks and reads all of it: errors are fine, panics and hangs are not.
    #[test]
    fn random_damage_neither_panics_nor_runs_away() {
        let rounds: u64 = std::env::var("FUZZ_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        let mut ran = 0;
        let mut x = 0x2545f4914f6cdd1du64;
        let mut rnd = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        for (ufs2, big) in [(false, true), (false, false), (true, false), (true, true)] {
            let good = Img::populated(ufs2, big).b;
            let sb = if ufs2 { 65536 } else { 8192 };
            for _ in 0..rounds {
                let mut img = good.clone();
                for _ in 0..1 + rnd(40) {
                    let at = match rnd(3) {
                        0 => INODES + rnd(8 * 256),
                        1 => sb + rnd(1380),
                        _ => rnd(img.len()),
                    };
                    img[at] = rnd(256) as u8;
                }
                ran += 1;
                if let Ok(fs) = Ffs::open(img) {
                    walk(&fs);
                }
            }
        }
        eprintln!("ffs: {ran} damaged images");
    }

    fn walk<F: ReadFs>(fs: &F) {
        for p in ["big", "d", "d/x", "hello", "lnk", "lnk/x", "missing"] {
            let Ok(st) = fs.stat(p) else { continue };
            if let Ok(f) = fs.open(p) {
                let _ = disk_rofs::read_limited(f.as_ref(), st.size, 1 << 20);
            }
            let _ = fs.read_link(p);
        }
    }
}
