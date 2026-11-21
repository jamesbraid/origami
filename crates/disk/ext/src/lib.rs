//! Read-only ext2, ext3 and ext4: 32- and 64-byte group descriptors, flex_bg
//! and meta_bg, block maps and extent trees, linear directories (an htree is
//! read as a list, which shows every entry) and symlinks. Ported from
//! Instigator's `ext` package. The journal is not replayed, checksums are not
//! checked, and inline-data inodes are refused.

use disk_rofs::{invalid, not_found, DirEntry, FileType, ReadAt, ReadFs, Stat};
use std::io;

const SUPER_OFF: u64 = 1024;
const SUPER_LEN: usize = 1024;
const MAGIC: u16 = 0xef53;
const ROOT_INO: u32 = 2;
const MAX_GROUPS: u64 = 1 << 22;

const INCOMPAT_COMPRESS: u32 = 0x1;
const INCOMPAT_JOURNAL_DEV: u32 = 0x8;
const INCOMPAT_META_BG: u32 = 0x10;
const INCOMPAT_64BIT: u32 = 0x80;
const RO_SPARSE_SUPER: u32 = 0x1;
const RO_LARGE_FILE: u32 = 0x2;

const FLAG_EXTENTS: u32 = 0x80000;
const FLAG_INLINE: u32 = 0x10000000;
const EXTENT_MAGIC: u16 = 0xf30a;
/// The kernel's limit on extent tree depth.
const MAX_DEPTH: u16 = 5;
const N_DIRECT: usize = 12;
/// A sparse file may be larger than its file system; one this much larger is corrupt.
const MAX_FILE: u64 = 1 << 40;
const MAX_DIR: u64 = 64 << 20;
/// Records examined in one lookup.
const MAX_SCAN: u32 = 1 << 18;
const MAX_LINK: u64 = 4096;
const MAX_READ: u64 = 1 << 20;
const DIR_HDR: u64 = 8;

fn le16(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2)
        .map_or(0, |v| u16::from_le_bytes(v.try_into().unwrap()))
}
fn le32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4)
        .map_or(0, |v| u32::from_le_bytes(v.try_into().unwrap()))
}

struct Geo {
    inodes: u64,
    blocks: u64,
    bsize: u64,
    bits: u32,
    bpg: u64,
    ipg: u64,
    isize: u64,
    dsize: u64,
    first_data: u64,
    revision: u32,
    incompat: u32,
    ro_compat: u32,
    first_meta_bg: u64,
    groups: u64,
    desc_per_blk: u64,
}

struct Inode {
    mode: u16,
    size: u64,
    flags: u32,
    blocks: u64,
    acl: u64,
    /// The 60-byte block area: pointers, an extent root or a fast symlink.
    iblock: [u8; 60],
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

struct Extent {
    lblk: u64,
    n: u64,
    start: u64,
    uninit: bool,
}

pub struct Ext<D> {
    dev: D,
    g: Geo,
}

impl<D: ReadAt + Send + Sync> Ext<D> {
    /// Opens the file system at the start of `dev`.
    pub fn open(dev: D) -> io::Result<Self> {
        let mut b = [0u8; SUPER_LEN];
        dev.read_exact_at(SUPER_OFF, &mut b)?;
        if le16(&b, 0x38) != MAGIC {
            return Err(invalid(format!(
                "ext: bad superblock magic {:#x}",
                le16(&b, 0x38)
            )));
        }
        let log_block = le32(&b, 0x18);
        if log_block > 6 {
            return Err(invalid("ext: implausible block size"));
        }
        let rev = le32(&b, 0x4c);
        let (mut isize, mut incompat, mut ro_compat, mut first_meta_bg) = (128u64, 0, 0, 0);
        if rev >= 1 {
            isize = le16(&b, 0x58) as u64;
            incompat = le32(&b, 0x60);
            ro_compat = le32(&b, 0x64);
            first_meta_bg = le32(&b, 0x104) as u64;
        }
        let mut blocks = le32(&b, 4) as u64;
        let mut dsize = 32u64;
        if incompat & INCOMPAT_64BIT != 0 {
            blocks |= (le32(&b, 0x150) as u64) << 32;
            dsize = le16(&b, 0xfe) as u64;
        }
        let g = Geo {
            inodes: le32(&b, 0) as u64,
            blocks,
            bsize: 1024 << log_block,
            bits: 10 + log_block,
            bpg: le32(&b, 0x20) as u64,
            ipg: le32(&b, 0x28) as u64,
            isize,
            dsize,
            first_data: le32(&b, 0x14) as u64,
            revision: rev,
            incompat,
            ro_compat,
            first_meta_bg,
            groups: 0,
            desc_per_blk: 0,
        };
        if g.incompat & INCOMPAT_JOURNAL_DEV != 0 {
            return Err(invalid(
                "ext: an external journal device, not a file system",
            ));
        }
        if g.incompat & INCOMPAT_COMPRESS != 0 {
            return Err(invalid("ext: compressed file systems are not supported"));
        }
        let pow2 = |v: u64| v & v.wrapping_sub(1) == 0;
        if g.isize < 128 || g.isize > g.bsize || !pow2(g.isize) {
            return Err(invalid("ext: implausible inode size"));
        }
        if g.dsize < 32
            || g.dsize > 1024
            || !pow2(g.dsize)
            || (g.incompat & INCOMPAT_64BIT != 0 && g.dsize < 64)
        {
            return Err(invalid("ext: implausible group descriptor size"));
        }
        if g.blocks == 0 || g.bpg == 0 || g.ipg == 0 || g.inodes == 0 || g.first_data >= g.blocks {
            return Err(invalid("ext: empty or inconsistent geometry"));
        }
        // The inode bitmap is one block; the byte offsets must fit an i64.
        if g.ipg > g.bsize * 8
            || g.blocks > 1 << 48
            || g.blocks
                .checked_mul(g.bsize)
                .is_none_or(|n| n > i64::MAX as u64)
        {
            return Err(invalid("ext: geometry beyond what the format can address"));
        }
        let groups = (g.blocks - g.first_data).div_ceil(g.bpg);
        if groups > MAX_GROUPS || groups * g.ipg < g.inodes {
            return Err(invalid("ext: group count does not hold the inodes"));
        }
        let g = Geo {
            groups,
            desc_per_blk: g.bsize / g.dsize,
            ..g
        };
        let fs = Ext { dev, g };
        let root = fs.inode(ROOT_INO)?;
        if root.file_type() != FileType::Dir {
            return Err(invalid("ext: root inode is not a directory"));
        }
        Ok(fs)
    }

    /// Reads `n` bytes at block `blk`; a block outside the file system is
    /// corruption whatever the device holds there.
    fn read_blocks(&self, blk: u64, n: u64) -> io::Result<Vec<u8>> {
        let g = &self.g;
        if blk >= g.blocks || n.div_ceil(g.bsize) > g.blocks - blk {
            return Err(invalid(format!("ext: block {blk} outside the file system")));
        }
        let mut b = vec![0; n as usize];
        self.dev.read_exact_at(blk * g.bsize, &mut b)?;
        Ok(b)
    }

    /// Whether group `n` carries a superblock backup, which a meta group's
    /// descriptor block sits after.
    fn has_super(&self, n: u64) -> bool {
        if n <= 1 || self.g.ro_compat & RO_SPARSE_SUPER == 0 {
            return true;
        }
        n & 1 == 1
            && [3u64, 5, 7].iter().any(|&p| {
                (1..)
                    .map(|k| p.pow(k))
                    .take_while(|&v| v <= n)
                    .any(|v| v == n)
            })
    }

    fn desc_block(&self, group: u64) -> u64 {
        let g = &self.g;
        let idx = group / g.desc_per_blk;
        if g.incompat & INCOMPAT_META_BG == 0 || idx < g.first_meta_bg {
            return g.first_data + idx + 1;
        }
        let bg = idx * g.desc_per_blk;
        g.first_data + bg * g.bpg + self.has_super(bg) as u64
    }

    fn inode_table(&self, group: u64) -> io::Result<u64> {
        let g = &self.g;
        if group >= g.groups {
            return Err(invalid(format!("ext: block group {group} of {}", g.groups)));
        }
        let blk = self.read_blocks(self.desc_block(group), g.bsize)?;
        let d = (group % g.desc_per_blk * g.dsize) as usize;
        let mut t = le32(&blk, d + 8) as u64;
        if g.incompat & INCOMPAT_64BIT != 0 && g.dsize >= 64 {
            t |= (le32(&blk, d + 0x28) as u64) << 32;
        }
        Ok(t)
    }

    fn inode(&self, n: u32) -> io::Result<Inode> {
        let g = &self.g;
        let n = n as u64;
        if n < 1 || n > g.inodes {
            return Err(invalid(format!("ext: inode {n} out of range")));
        }
        let (group, idx) = ((n - 1) / g.ipg, (n - 1) % g.ipg);
        let table = self.inode_table(group)?;
        if table == 0 || table >= g.blocks {
            return Err(invalid(format!(
                "ext: group {group} has its inode table at block {table}"
            )));
        }
        let mut raw = vec![0u8; g.isize as usize];
        self.dev
            .read_exact_at(table * g.bsize + idx * g.isize, &mut raw)?;
        let mode = le16(&raw, 0);
        let mut size = le32(&raw, 4) as u64;
        // i_size_high is a directory ACL in the first revision.
        if mode & 0o170000 == 0o100000 && (g.revision >= 1 || g.ro_compat & RO_LARGE_FILE != 0) {
            size |= (le32(&raw, 0x6c) as u64) << 32;
        }
        let blocks = le32(&raw, 0x1c) as u64;
        let mut iblock = [0; 60];
        iblock.copy_from_slice(
            raw.get(0x28..0x28 + 60)
                .ok_or_else(|| invalid("ext: short inode"))?,
        );
        if size > i64::MAX as u64 || (size > MAX_FILE && size / 16 > g.blocks * g.bsize) {
            return Err(invalid(format!("ext: inode {n} size {size}")));
        }
        Ok(Inode {
            mode,
            size,
            flags: le32(&raw, 0x20),
            blocks,
            acl: le32(&raw, 0x68) as u64 | (le16(&raw, 0x76) as u64) << 32,
            iblock,
        })
    }

    /// The extent holding logical block `lbn`, if any, and the first logical
    /// block after it that might be mapped, so a hole can be stepped over.
    fn extent_run(&self, ino: &Inode, lbn: u64) -> io::Result<(Option<Extent>, u64)> {
        let mut next = 1u64 << 32;
        let mut node = ino.iblock.to_vec();
        let mut depth: Option<u16> = None;
        loop {
            if node.len() < 12 || le16(&node, 0) != EXTENT_MAGIC {
                return Err(invalid("ext: bad extent header"));
            }
            let entries = le16(&node, 2) as usize;
            let d = le16(&node, 6);
            if d > MAX_DEPTH
                || depth.is_some_and(|p| p == 0 || d != p - 1)
                || 12 + entries * 12 > node.len()
            {
                return Err(invalid("ext: implausible extent node"));
            }
            depth = Some(d);
            // Entries are sorted by logical block: take the last that starts at or before lbn.
            let at = (0..entries)
                .take_while(|&i| le32(&node, 12 + i * 12) as u64 <= lbn)
                .last();
            let after = at.map_or(0, |a| a + 1);
            if after < entries {
                next = next.min(le32(&node, 12 + after * 12) as u64);
            }
            let Some(at) = at else {
                return Ok((None, next));
            };
            let e = 12 + at * 12;
            if d == 0 {
                let mut x = Extent {
                    lblk: le32(&node, e) as u64,
                    n: le16(&node, e + 4) as u64,
                    start: le32(&node, e + 8) as u64 | (le16(&node, e + 6) as u64) << 32,
                    uninit: false,
                };
                if x.n > 32768 {
                    x.n -= 32768;
                    x.uninit = true;
                }
                return Ok(if lbn >= x.lblk + x.n {
                    (None, next)
                } else {
                    (Some(x), next)
                });
            }
            let child = le32(&node, e + 4) as u64 | (le16(&node, e + 8) as u64) << 32;
            node = self.read_blocks(child, self.g.bsize)?;
        }
    }

    /// The disk block of logical block `lbn` of a block-mapped file, 0 for a hole.
    fn map_block(&self, ino: &Inode, mut lbn: u64) -> io::Result<u64> {
        if lbn < N_DIRECT as u64 {
            return Ok(le32(&ino.iblock, lbn as usize * 4) as u64);
        }
        lbn -= N_DIRECT as u64;
        let per = self.g.bsize / 4;
        let mut span = 1u64;
        for lvl in 0..3 {
            span *= per;
            if lbn >= span {
                lbn -= span;
                continue;
            }
            let mut p = le32(&ino.iblock, (N_DIRECT + lvl) * 4) as u64;
            for _ in 0..=lvl {
                if p == 0 {
                    break;
                }
                let b = self.read_blocks(p, self.g.bsize)?;
                span /= per;
                p = le32(&b, (lbn / span * 4) as usize) as u64;
                lbn %= span;
            }
            return Ok(p);
        }
        Err(invalid("ext: block beyond the triple indirect range"))
    }

    fn read_inode_at(&self, ino: &Inode, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        let g = &self.g;
        if ino.flags & FLAG_INLINE != 0 {
            return Err(invalid("ext: inline-data inodes are not supported"));
        }
        if pos >= ino.size {
            return Ok(0);
        }
        let want = buf
            .len()
            .min((ino.size - pos).min(usize::MAX as u64) as usize);
        let mut done = 0;
        while done < want {
            let at = pos + done as u64;
            let (lbn, within) = (at >> g.bits, at & (g.bsize - 1));
            let left = (want - done) as u64;
            let (phys, run) = if ino.flags & FLAG_EXTENTS != 0 {
                match self.extent_run(ino, lbn)? {
                    (None, next) => (0, next.saturating_sub(lbn)),
                    (Some(e), _) => {
                        let run = (e.lblk + e.n).saturating_sub(lbn);
                        (
                            if e.uninit {
                                0
                            } else {
                                e.start + (lbn - e.lblk)
                            },
                            run,
                        )
                    }
                }
            } else {
                (self.map_block(ino, lbn)?, 1)
            };
            if run == 0 {
                return Err(invalid("ext: empty block mapping"));
            }
            let seg = left.min(run.saturating_mul(g.bsize) - within).min(MAX_READ);
            let out = &mut buf[done..done + seg as usize];
            if phys == 0 {
                out.fill(0); // hole or uninitialised extent
            } else {
                if phys >= g.blocks || (within + seg).div_ceil(g.bsize) > g.blocks - phys {
                    return Err(invalid(format!(
                        "ext: block {phys} outside the file system"
                    )));
                }
                self.dev.read_exact_at(phys * g.bsize + within, out)?;
            }
            done += seg as usize;
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
            let m = (size - at as u64).min(MAX_READ) as usize;
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
            return Err(invalid("ext: not a directory"));
        }
        let bs = self.g.bsize;
        let size = dir.size.min(MAX_DIR);
        let mut buf = vec![0u8; bs as usize];
        let (mut pos, mut scanned) = (0, 0);
        while pos < size {
            let n = (size - pos).min(bs) as usize;
            if !matches!(self.read_inode_at(dir, pos, &mut buf[..n]), Ok(m) if m == n) {
                return Ok(None);
            }
            let blk = &buf[..n];
            let mut off = 0u64;
            while off + DIR_HDR <= n as u64 {
                scanned += 1;
                let o = off as usize;
                let ino = le32(blk, o);
                let rl = rec_len(le16(blk, o + 4), bs);
                let nl = blk[o + 6] as u64;
                // A record that cannot be followed costs the rest of its block.
                if scanned > MAX_SCAN
                    || rl < DIR_HDR
                    || !rl.is_multiple_of(4)
                    || off + rl > n as u64
                    || DIR_HDR + nl > rl
                {
                    break;
                }
                let found = &blk[o + 8..o + 8 + nl as usize];
                off += rl;
                if ino != 0 && found == name.as_bytes() {
                    return Ok(Some(ino));
                }
            }
            if scanned > MAX_SCAN {
                return Ok(None);
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

/// A record length, which 16 bits cannot hold for blocks of 64 KiB or more.
fn rec_len(v: u16, bs: u64) -> u64 {
    if bs < 1 << 16 {
        v as u64
    } else if v == u16::MAX || v == 0 {
        bs
    } else {
        (v & 0xfffc) as u64 | ((v & 3) as u64) << 16
    }
}

struct File<'a, D> {
    fs: &'a Ext<D>,
    ino: Inode,
}

impl<D: ReadAt + Send + Sync> ReadAt for File<'_, D> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.fs.read_inode_at(&self.ino, pos, buf)
    }
}

impl<D: ReadAt + Send + Sync> ReadFs for Ext<D> {
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
            "ext: no directory listing",
        ))
    }

    fn read_link(&self, path: &str) -> io::Result<String> {
        let i = self.lookup(path)?;
        if i.file_type() != FileType::Symlink {
            return Err(invalid("ext: not a symlink"));
        }
        if i.size > MAX_LINK {
            return Err(invalid("ext: symlink target too long"));
        }
        // A fast symlink keeps its target in the block area and owns no
        // blocks; an extended-attribute block counts as one.
        let mut own = i.blocks;
        if i.acl != 0 {
            own = own.saturating_sub(self.g.bsize / 512);
        }
        let fast = i.flags & (FLAG_EXTENTS | FLAG_INLINE) == 0 && i.size < 60 && own == 0;
        let bytes = if fast {
            i.iblock[..i.size as usize].to_vec()
        } else {
            self.read_all(&i, MAX_LINK)?
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BS: usize = 1024;
    const TABLE: usize = 5 * BS;

    struct Img(Vec<u8>);

    impl Img {
        fn put(&mut self, o: usize, v: u64, n: usize) {
            self.0[o..o + n].copy_from_slice(&v.to_le_bytes()[..n]);
        }

        fn inode(&mut self, n: usize, mode: u16, size: u32, flags: u32, iblock: &[u8]) {
            let o = TABLE + (n - 1) * 128;
            self.put(o, mode as u64, 2);
            self.put(o + 4, size as u64, 4);
            self.put(o + 0x20, flags as u64, 4);
            self.0[o + 0x28..o + 0x28 + iblock.len()].copy_from_slice(iblock);
        }

        fn dir(&mut self, blk: usize, ents: &[(&str, u32)]) {
            let mut p = 0;
            for (k, (name, ino)) in ents.iter().enumerate() {
                let mut len = (8 + name.len()).next_multiple_of(4);
                if k + 1 == ents.len() {
                    len = BS - p;
                }
                let o = blk * BS + p;
                self.put(o, *ino as u64, 4);
                self.put(o + 4, len as u64, 2);
                self.0[o + 6] = name.len() as u8;
                self.0[o + 8..o + 8 + name.len()].copy_from_slice(name.as_bytes());
                p += len;
            }
        }

        /// 128 one-KiB blocks, one group: inode table at block 5, data from 10.
        fn new() -> Img {
            let mut i = Img(vec![0; 128 * BS]);
            for (o, v, n) in [
                (0, 32, 4),
                (4, 128, 4),
                (0x14, 1, 4),
                (0x20, 8192, 4),
                (0x28, 32, 4),
                (0x38, MAGIC as u64, 2),
                (0x4c, 1, 4),
                (0x58, 128, 2),
                (0x60, 0x42, 4), // filetype, extents
            ] {
                i.put(BS + o, v, n);
            }
            i.put(2 * BS + 8, 5, 4); // group 0's inode table
            i
        }

        fn extent_header(entries: u16, depth: u16) -> Vec<u8> {
            let mut h = vec![0x0a, 0xf3];
            h.extend(entries.to_le_bytes());
            h.extend(4u16.to_le_bytes());
            h.extend(depth.to_le_bytes());
            h.extend([0; 4]);
            h
        }

        fn extent(lblk: u32, len: u16, start: u32) -> Vec<u8> {
            let mut e = lblk.to_le_bytes().to_vec();
            e.extend(len.to_le_bytes());
            e.extend(0u16.to_le_bytes());
            e.extend(start.to_le_bytes());
            e
        }

        /// /hello, /lnk -> abc, /big (13 mapped blocks, the last through the
        /// indirect block), /ext (one extent), /deep (an extent tree of depth 1 with a hole).
        fn populated() -> Img {
            let mut i = Img::new();
            i.inode(2, 0o40755, BS as u32, 0, &10u32.to_le_bytes());
            i.dir(
                10,
                &[
                    (".", 2),
                    ("..", 2),
                    ("hello", 3),
                    ("lnk", 4),
                    ("big", 5),
                    ("ext", 6),
                    ("deep", 7),
                ],
            );
            // the block pointers of a classic file system; /hello uses block 11
            i.put(TABLE + 2 * 128 + 0x20, 0, 4);
            i.inode(3, 0o100644, 5, 0, &11u32.to_le_bytes());
            i.0[11 * BS..11 * BS + 5].copy_from_slice(b"world");
            i.inode(4, 0o120777, 3, 0, b"abc");
            let mut ptrs: Vec<u8> = (0..12u32).flat_map(|k| (12 + k).to_le_bytes()).collect();
            ptrs.extend(24u32.to_le_bytes());
            i.inode(5, 0o100644, 13 * BS as u32, 0, &ptrs);
            for k in 0..12 {
                i.0[(12 + k) * BS..][..BS].fill(k as u8 + 1);
            }
            i.put(24 * BS, 25, 4);
            i.0[25 * BS..][..BS].fill(13);
            let mut root = Img::extent_header(1, 0);
            root.extend(Img::extent(0, 2, 30));
            i.inode(6, 0o100644, 2 * BS as u32, FLAG_EXTENTS, &root);
            i.0[30 * BS..][..2 * BS].fill(0xee);
            let mut root = Img::extent_header(1, 1);
            root.extend(0u32.to_le_bytes());
            root.extend(32u32.to_le_bytes());
            root.extend([0; 4]);
            i.inode(7, 0o100644, 5 * BS as u32, FLAG_EXTENTS, &root);
            let mut leaf = Img::extent_header(2, 0);
            leaf.extend(Img::extent(0, 2, 34));
            leaf.extend(Img::extent(4, 1, 36));
            i.0[32 * BS..32 * BS + leaf.len()].copy_from_slice(&leaf);
            i.0[34 * BS..][..2 * BS].fill(0x11);
            i.0[36 * BS..][..BS].fill(0x22);
            i
        }
    }

    fn at(f: &dyn ReadAt, pos: u64) -> u8 {
        let mut b = [0];
        f.read_exact_at(pos, &mut b).unwrap();
        b[0]
    }

    #[test]
    fn reads_files_dirs_links_maps_and_extents() {
        let fs = Ext::open(Img::populated().0).unwrap();
        for (path, ty) in [
            ("big", FileType::File),
            ("deep", FileType::File),
            ("ext", FileType::File),
            ("hello", FileType::File),
            ("lnk", FileType::Symlink),
        ] {
            assert_eq!(fs.stat(path).unwrap().file_type, ty, "{path}");
        }
        assert!(fs.read_dir(".").is_err());
        let mut b = [0; 5];
        fs.open("hello").unwrap().read_exact_at(0, &mut b).unwrap();
        assert_eq!(&b, b"world");
        assert_eq!(fs.read_link("lnk").unwrap(), "abc");
        assert!(fs.stat("lnk/x").is_err());
        let big = fs.open("big").unwrap();
        assert_eq!(
            [at(&*big, 5), at(&*big, 11 * 1024), at(&*big, 12 * 1024 + 7)],
            [1, 12, 13]
        );
        assert_eq!(at(&*fs.open("ext").unwrap(), 2047), 0xee);
        let deep = fs.open("deep").unwrap();
        assert_eq!(
            [
                at(&*deep, 0),
                at(&*deep, 2048),
                at(&*deep, 3071),
                at(&*deep, 4096)
            ],
            [0x11, 0, 0, 0x22]
        );
        assert!(fs.stat("missing").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(Ext::open(vec![0u8; 1 << 16]).is_err());
        assert!(Ext::open(vec![0u8; 100]).is_err());
    }

    #[test]
    fn hostile_geometry_is_refused() {
        let patch = |at: usize, v: u64, n: usize| {
            let mut i = Img::populated();
            i.put(BS + at, v, n);
            i.0
        };
        for (what, img) in [
            ("block size", patch(0x18, 20, 4)),
            ("inode size", patch(0x58, 100, 2)),
            ("no blocks per group", patch(0x20, 0, 4)),
            ("inodes per group over a bitmap", patch(0x28, 1 << 20, 4)),
            ("first data block past the end", patch(0x14, 1 << 30, 4)),
            ("external journal", patch(0x60, 0x48, 4)),
            ("huge 64-bit block count", patch(0x60, 0xc2, 4)),
        ] {
            assert!(Ext::open(img).is_err(), "{what}");
        }
    }

    #[test]
    fn hostile_mappings_are_errors_not_reads() {
        let mut i = Img::populated();
        i.put(TABLE + 2 * 128 + 0x28, 0xffff_fff0, 4); // /hello's block
        let fs = Ext::open(i.0.clone()).unwrap();
        assert!(fs
            .open("hello")
            .unwrap()
            .read_exact_at(0, &mut [0; 5])
            .is_err());
        // an extent node that claims depth 5 under a depth 1 parent
        let leaf = 32 * BS;
        i.put(leaf + 6, 5, 2);
        let fs = Ext::open(i.0.clone()).unwrap();
        assert!(fs
            .open("deep")
            .unwrap()
            .read_exact_at(0, &mut [0; 1])
            .is_err());
        // a size of u64::MAX in the high word is corrupt, not a terabyte read
        i.put(TABLE + 2 * 128 + 0x6c, u32::MAX as u64, 4);
        i.put(TABLE + 2 * 128 + 4, u32::MAX as u64, 4);
        let fs = Ext::open(i.0).unwrap();
        assert!(fs.stat("hello").is_err());
    }

    #[test]
    fn damaged_directory_block_is_skipped() {
        let mut i = Img::populated();
        i.put(10 * BS + 4, 0, 2);
        let fs = Ext::open(i.0).unwrap();
        assert!(fs.stat("hello").is_err());
    }

    #[test]
    fn huge_directory_is_scanned_in_bounded_memory() {
        let mut i = Img::populated();
        i.put(TABLE + 128 + 4, 60 << 20, 4); // root: 60 MiB of block 10 and holes
        let fs = Ext::open(i.0).unwrap();
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
            .unwrap_or(500);
        let good = Img::populated().0;
        let mut x = 0x9e3779b97f4a7c15u64;
        let mut rnd = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        let mut ran = 0;
        for _ in 0..rounds {
            let mut img = good.clone();
            for _ in 0..1 + rnd(40) {
                let at = match rnd(3) {
                    0 => TABLE + rnd(8 * 128),
                    1 => BS + rnd(0x200),
                    _ => rnd(img.len()),
                };
                img[at] = rnd(256) as u8;
            }
            ran += 1;
            if let Ok(fs) = Ext::open(img) {
                walk(&fs);
            }
        }
        eprintln!("ext: {ran} damaged images");
    }

    fn walk<F: ReadFs>(fs: &F) {
        for p in ["big", "deep", "ext", "hello", "lnk", "lnk/x", "missing"] {
            let Ok(st) = fs.stat(p) else { continue };
            if let Ok(f) = fs.open(p) {
                let _ = disk_rofs::read_limited(f.as_ref(), st.size, 1 << 20);
            }
            let _ = fs.read_link(p);
        }
    }
}
