//! Read-only XFS as IRIX wrote it: superblock versions 1 to 4, version 1
//! and 2 inodes, version 1 and 2 directories. No CRC (version 5) layout,
//! realtime files, attributes or log. Ported from Instigator's `xfs`
//! package on the `cab-lzx` branch.

use disk_rofs::{invalid, not_found, DirEntry, FileType, ReadAt, ReadFs, Stat};
use std::collections::HashSet;
use std::io;
use std::sync::OnceLock;

const SUPER_MAGIC: u32 = 0x5846_5342;
const VER_DIRV2: u16 = 0x2000;
const VER_MOREBITS: u16 = 0x8000;
const FEATURES2_FTYPE: u32 = 0x200;
const MAX_SLURP: u64 = 64 << 20;
const MAX_EXTENTS: usize = 1 << 22;

const FMT_LOCAL: u8 = 1;
const FMT_EXTENTS: u8 = 2;
const FMT_BTREE: u8 = 3;

const DIR2_BLOCK: u32 = 0x5844_3242;
const DIR2_DATA: u32 = 0x5844_3244;
const DIR1_LEAF: u16 = 0xfeeb;
/// File offset where version 2 leaf and free-index blocks start.
const DIR2_LEAF_OFF: u64 = 1 << 35;

pub struct Xfs<D> {
    dev: D,
    block_size: u64,
    inode_size: u64,
    ino_per_block_log: u32,
    ag_blk_log: u32,
    ag_blocks: u64,
    ag_count: u64,
    root_ino: u64,
    dir_blk_size: u64,
    dir_v2: bool,
    ftype: bool,
    pub label: String,
}

#[derive(Clone, Copy)]
struct Extent {
    off: u64,
    start: u64,
    count: u64,
    unwritten: bool,
}

struct Inode {
    mode: u16,
    format: u8,
    size: u64,
    nexts: u64,
    fork: Vec<u8>,
    exts: OnceLock<Vec<Extent>>,
}

impl Inode {
    fn file_type(&self) -> FileType {
        match self.mode & 0xf000 {
            0x8000 => FileType::File,
            0x4000 => FileType::Dir,
            0xa000 => FileType::Symlink,
            _ => FileType::Other,
        }
    }
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes(b[o..o + 2].try_into().unwrap())
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

fn decode_extent(b: &[u8]) -> Extent {
    let (l0, l1) = (be64(b, 0), be64(b, 8));
    Extent {
        unwritten: l0 >> 63 != 0,
        off: (l0 >> 9) & ((1 << 54) - 1),
        start: (l0 & 0x1ff) << 43 | l1 >> 21,
        count: l1 & ((1 << 21) - 1),
    }
}

impl<D: ReadAt + Send + Sync> Xfs<D> {
    /// Opens the file system at the start of `dev`.
    pub fn open(dev: D) -> io::Result<Self> {
        let mut b = [0u8; 512];
        dev.read_exact_at(0, &mut b)?;
        if be32(&b, 0) != SUPER_MAGIC {
            return Err(invalid(format!(
                "xfs: bad superblock magic {:#x}",
                be32(&b, 0)
            )));
        }
        let version = be16(&b, 100);
        if !(1..=4).contains(&(version & 0xf)) {
            return Err(invalid(format!(
                "xfs: unsupported superblock version {}",
                version & 0xf
            )));
        }
        let features2 = if version & VER_MOREBITS != 0 {
            be32(&b, 200)
        } else {
            0
        };
        let block_size = be32(&b, 4) as u64;
        let inode_size = be16(&b, 104) as u64;
        let ag_blocks = be32(&b, 84) as u64;
        let ag_count = be32(&b, 88) as u64;
        if !(512..=65536).contains(&block_size)
            || !block_size.is_power_of_two()
            || !(128..=4096).contains(&inode_size)
            || !inode_size.is_power_of_two()
            || inode_size > block_size
            || ag_blocks == 0
            || ag_count == 0
            || b[192] > 8
            || be64(&b, 56) == 0
        {
            return Err(invalid("xfs: implausible superblock"));
        }
        let label = &b[108..120];
        let label = &label[..label.iter().position(|&c| c == 0).unwrap_or(12)];
        Ok(Xfs {
            dev,
            block_size,
            inode_size,
            ino_per_block_log: (block_size / inode_size).trailing_zeros(),
            ag_blk_log: {
                // The stored log may exceed the minimum but not 40.
                let want = u64::BITS - (ag_blocks - 1).leading_zeros();
                let stored = b[124] as u32;
                if stored < want || stored > 40 {
                    want
                } else {
                    stored
                }
            },
            ag_blocks,
            ag_count,
            root_ino: be64(&b, 56),
            dir_blk_size: block_size << b[192],
            dir_v2: version & VER_DIRV2 != 0,
            ftype: features2 & FEATURES2_FTYPE != 0,
            label: String::from_utf8_lossy(label).into(),
        })
    }

    /// Byte offset of a packed filesystem block number.
    fn block_offset(&self, fsb: u64) -> io::Result<u64> {
        let (ag, bno) = (fsb >> self.ag_blk_log, fsb & ((1 << self.ag_blk_log) - 1));
        if ag >= self.ag_count || bno >= self.ag_blocks {
            return Err(invalid(format!(
                "xfs: block {fsb:#x} outside the file system"
            )));
        }
        self.byte_offset(ag, bno)
    }

    fn byte_offset(&self, ag: u64, bno: u64) -> io::Result<u64> {
        (ag * self.ag_blocks + bno)
            .checked_mul(self.block_size)
            .ok_or_else(|| invalid("xfs: block offset overflows"))
    }

    fn read_blocks(&self, fsb: u64, n: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0; n * self.block_size as usize];
        self.dev.read_exact_at(self.block_offset(fsb)?, &mut v)?;
        Ok(v)
    }

    fn inode(&self, ino: u64) -> io::Result<Inode> {
        let log = self.ino_per_block_log;
        let (idx, bno, ag) = (
            ino & ((1 << log) - 1),
            (ino >> log) & ((1 << self.ag_blk_log) - 1),
            ino >> (log + self.ag_blk_log),
        );
        if ag >= self.ag_count || bno >= self.ag_blocks || idx >= self.block_size / self.inode_size
        {
            return Err(invalid(format!("xfs: inode {ino} outside the file system")));
        }
        let off = self.byte_offset(ag, bno)? + idx * self.inode_size;
        let mut b = vec![0; self.inode_size as usize];
        self.dev.read_exact_at(off, &mut b)?;
        if be16(&b, 0) != 0x494e {
            return Err(invalid(format!("xfs: inode {ino}: bad magic")));
        }
        if !(1..=2).contains(&b[4]) {
            return Err(invalid(format!(
                "xfs: inode {ino}: unsupported version {}",
                b[4]
            )));
        }
        let forkoff = b[82] as usize;
        let end = if forkoff != 0 {
            (100 + forkoff * 8).min(b.len())
        } else {
            b.len()
        };
        let size = be64(&b, 56);
        if size > 1 << 40 {
            return Err(invalid(format!("xfs: inode {ino}: implausible size")));
        }
        Ok(Inode {
            mode: be16(&b, 2),
            format: b[5],
            size,
            nexts: be32(&b, 76) as u64,
            fork: b[100..end].to_vec(),
            exts: OnceLock::new(),
        })
    }

    /// The block map in file order with overlapping runs dropped, decoded
    /// once per inode.
    fn extents<'a>(&self, d: &'a Inode) -> io::Result<&'a [Extent]> {
        if let Some(e) = d.exts.get() {
            return Ok(e);
        }
        let mut raw = vec![];
        match d.format {
            FMT_EXTENTS => {
                let n = (d.nexts as usize).min(d.fork.len() / 16);
                raw.extend((0..n).map(|i| decode_extent(&d.fork[i * 16..])));
            }
            FMT_BTREE => self.btree_extents(&d.fork, &mut raw)?,
            _ => {}
        }
        raw.retain(|e| e.count != 0 && e.off < 1 << 46 && e.count < 1 << 46);
        raw.sort_by_key(|e| e.off);
        let mut end = 0;
        raw.retain(|e| {
            let keep = e.off >= end;
            if keep {
                end = e.off + e.count;
            }
            keep
        });
        Ok(d.exts.get_or_init(|| raw))
    }

    fn btree_extents(&self, root: &[u8], out: &mut Vec<Extent>) -> io::Result<()> {
        if root.len() < 4 {
            return Err(invalid("xfs: btree root does not fit"));
        }
        let level = be16(root, 0) as usize;
        let maxrecs = (root.len() - 4) / 16;
        let nrecs = (be16(root, 2) as usize).min(maxrecs);
        if level == 0 || level > 8 {
            return Err(invalid(format!("xfs: btree depth {level}")));
        }
        let ptrs = &root[4 + maxrecs * 8..];
        let mut seen = HashSet::new();
        let mut stack: Vec<(u64, usize)> = (0..nrecs)
            .rev()
            .map(|i| (be64(ptrs, i * 8), level - 1))
            .collect();
        while let Some((fsb, level)) = stack.pop() {
            if out.len() > MAX_EXTENTS || !seen.insert(fsb) {
                continue;
            }
            let blk = self.read_blocks(fsb, 1)?;
            if be32(&blk, 0) != 0x424d_4150 || be16(&blk, 4) as usize != level {
                return Err(invalid("xfs: bad block-map btree block"));
            }
            let n = be16(&blk, 6) as usize;
            let maxr = (blk.len() - 24) / 16;
            let n = n.min(maxr);
            if level == 0 {
                out.extend((0..n).map(|i| decode_extent(&blk[24 + i * 16..])));
            } else {
                let p = &blk[24 + maxr * 8..];
                stack.extend((0..n).rev().map(|i| (be64(p, i * 8), level - 1)));
            }
        }
        Ok(())
    }

    /// Reads file data; holes and unwritten extents read as zeros. A
    /// directory's `size` is overridden by `limit` for its trailing blocks.
    fn read_inode_at(&self, d: &Inode, pos: u64, buf: &mut [u8], limit: u64) -> io::Result<usize> {
        if pos >= limit {
            return Ok(0);
        }
        let want = buf.len().min((limit - pos) as usize);
        let buf = &mut buf[..want];
        if d.format == FMT_LOCAL {
            let src = d.fork.get(pos as usize..).unwrap_or(&[]);
            let n = src.len().min(want);
            buf[..n].copy_from_slice(&src[..n]);
            buf[n..].fill(0);
            return Ok(want);
        }
        let exts = self.extents(d)?;
        let bs = self.block_size;
        let mut done = 0;
        while done < want {
            let at = pos + done as u64;
            let blk = at / bs;
            let i = exts.partition_point(|e| e.off + e.count <= blk);
            let hit = exts.get(i).filter(|e| e.off <= blk);
            let n = match hit {
                None => {
                    let next = exts.get(i).map_or(u64::MAX, |e| e.off * bs - at);
                    let n = (want - done).min(next.min(usize::MAX as u64) as usize);
                    buf[done..done + n].fill(0);
                    n
                }
                Some(e) => {
                    let within = at - e.off * bs;
                    let n = (want - done).min((e.count * bs - within) as usize);
                    if e.unwritten {
                        buf[done..done + n].fill(0);
                    } else {
                        self.dev.read_exact_at(
                            self.block_offset(e.start)? + within,
                            &mut buf[done..done + n],
                        )?;
                    }
                    n
                }
            };
            done += n;
        }
        Ok(want)
    }

    fn slurp(&self, d: &Inode) -> io::Result<Vec<u8>> {
        if d.size > MAX_SLURP {
            return Err(invalid("xfs: file too large to read whole"));
        }
        let mut v = vec![0; d.size as usize];
        self.read_inode_at(d, 0, &mut v, d.size)?;
        Ok(v)
    }

    fn list(&self, d: &Inode) -> io::Result<Vec<(String, u64)>> {
        if d.file_type() != FileType::Dir {
            return Err(invalid("xfs: not a directory"));
        }
        match d.format {
            FMT_LOCAL if self.dir_v2 => self.short_form2(d),
            FMT_LOCAL => self.short_form1(d),
            FMT_EXTENTS | FMT_BTREE => self.block_dir(d),
            f => Err(invalid(format!("xfs: directory fork format {f}"))),
        }
    }

    fn short_form2(&self, d: &Inode) -> io::Result<Vec<(String, u64)>> {
        let f = &d.fork[..d.fork.len().min(d.size as usize)];
        let bad = || invalid("xfs: short-form directory entry runs past the inode");
        if f.len() < 6 {
            return Err(bad());
        }
        let ino_len = if f[1] != 0 { 8 } else { 4 };
        let mut pos = 2 + ino_len;
        let ft = self.ftype as usize;
        let mut out = vec![];
        for _ in 0..f[0] {
            let nl = *f.get(pos).ok_or_else(bad)? as usize;
            pos += 3;
            if pos + nl + ft + ino_len > f.len() {
                return Err(bad());
            }
            let name = String::from_utf8_lossy(&f[pos..pos + nl]).into_owned();
            pos += nl + ft;
            let ino = if ino_len == 8 {
                be64(f, pos)
            } else {
                be32(f, pos) as u64
            };
            pos += ino_len;
            out.push((name, ino));
        }
        Ok(out)
    }

    fn short_form1(&self, d: &Inode) -> io::Result<Vec<(String, u64)>> {
        let f = &d.fork[..d.fork.len().min(d.size as usize)];
        let bad = || invalid("xfs: short-form directory entry runs past the inode");
        if f.len() < 9 {
            return Err(bad());
        }
        let mut pos = 9;
        let mut out = vec![];
        for _ in 0..f[8] {
            if pos + 9 > f.len() || pos + 9 + f[pos + 8] as usize > f.len() {
                return Err(bad());
            }
            let nl = f[pos + 8] as usize;
            out.push((
                String::from_utf8_lossy(&f[pos + 9..pos + 9 + nl]).into_owned(),
                be64(f, pos),
            ));
            pos += 9 + nl;
        }
        Ok(out)
    }

    /// Names live in the data blocks (version 2, from the start of the
    /// file) or the leaf blocks (version 1); the hash index is not needed.
    fn block_dir(&self, d: &Inode) -> io::Result<Vec<(String, u64)>> {
        let bs = self.block_size;
        let dbs = if self.dir_v2 { self.dir_blk_size } else { bs };
        let mut blocks = vec![];
        for e in self.extents(d)? {
            if self.dir_v2 && e.off * bs >= DIR2_LEAF_OFF {
                break;
            }
            let (first, last) = (e.off * bs / dbs, ((e.off + e.count) * bs - 1) / dbs);
            let from = blocks.last().map_or(first, |l| first.max(l + 1));
            blocks.extend((from..=last).take((1 << 16) - blocks.len()));
        }
        let mut out = vec![];
        let mut buf = vec![0; dbs as usize];
        for b in blocks {
            self.read_inode_at(d, b * dbs, &mut buf, u64::MAX)?;
            if self.dir_v2 {
                self.data_block2(&buf, &mut out)?;
            } else {
                Self::leaf_block1(&buf, &mut out)?;
            }
        }
        Ok(out)
    }

    fn data_block2(&self, b: &[u8], out: &mut Vec<(String, u64)>) -> io::Result<()> {
        let magic = be32(b, 0);
        let mut end = b.len();
        match magic {
            DIR2_DATA => {}
            DIR2_BLOCK => {
                end = b
                    .len()
                    .saturating_sub(8 + be32(b, b.len() - 8) as usize * 8);
                if end < 16 {
                    return Err(invalid("xfs: block directory tail is implausible"));
                }
            }
            m => return Err(invalid(format!("xfs: bad directory block magic {m:#x}"))),
        }
        let ft = self.ftype as usize;
        let mut pos = 16;
        while pos + 9 <= end {
            if be16(b, pos) == 0xffff {
                let n = be16(b, pos + 2) as usize;
                if n < 8 || !n.is_multiple_of(8) || pos + n > end {
                    return Err(invalid("xfs: bad free-space tag in a directory"));
                }
                pos += n;
                continue;
            }
            let nl = b[pos + 8] as usize;
            let size = (8 + 1 + nl + ft + 2 + 7) & !7;
            if pos + size > end {
                return Err(invalid("xfs: directory entry runs past the block"));
            }
            let name = String::from_utf8_lossy(&b[pos + 9..pos + 9 + nl]).into_owned();
            if name != "." && name != ".." {
                out.push((name, be64(b, pos)));
            }
            pos += size;
        }
        Ok(())
    }

    fn leaf_block1(b: &[u8], out: &mut Vec<(String, u64)>) -> io::Result<()> {
        match be16(b, 8) {
            0xfebe => return Ok(()), // interior hash node
            DIR1_LEAF => {}
            m => return Err(invalid(format!("xfs: bad directory block magic {m:#x}"))),
        }
        for k in 0..be16(b, 12) as usize {
            let p = 32 + k * 8;
            if p + 8 > b.len() {
                return Err(invalid("xfs: directory leaf claims too many entries"));
            }
            let (idx, nl) = (be16(b, p + 4) as usize, b[p + 6] as usize);
            if idx < 32 || idx + 8 + nl > b.len() {
                return Err(invalid("xfs: directory leaf name outside the block"));
            }
            let name = String::from_utf8_lossy(&b[idx + 8..idx + 8 + nl]).into_owned();
            if name != "." && name != ".." {
                out.push((name, be64(b, idx)));
            }
        }
        Ok(())
    }

    fn lookup(&self, path: &str) -> io::Result<Inode> {
        let mut ino = self.inode(self.root_ino)?;
        let parts: Vec<_> = path
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        for (i, part) in parts.iter().enumerate() {
            let next = self
                .list(&ino)
                .map_err(|_| not_found(path))?
                .into_iter()
                .find(|(n, _)| n == part)
                .ok_or_else(|| not_found(path))?;
            ino = self.inode(next.1)?;
            if ino.file_type() == FileType::Symlink && i + 1 < parts.len() {
                return Err(invalid(format!("{path}: symlink in path")));
            }
        }
        Ok(ino)
    }
}

struct File<'a, D> {
    fs: &'a Xfs<D>,
    ino: Inode,
}

impl<D: ReadAt + Send + Sync> ReadAt for File<'_, D> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.fs.read_inode_at(&self.ino, pos, buf, self.ino.size)
    }
}

impl<D: ReadAt + Send + Sync> ReadFs for Xfs<D> {
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

    fn read_dir(&self, path: &str) -> io::Result<Vec<DirEntry>> {
        let dir = self.lookup(path)?;
        Ok(self
            .list(&dir)?
            .into_iter()
            .map(|(name, ino)| {
                let file_type = self
                    .inode(ino)
                    .map(|i| i.file_type())
                    .unwrap_or(FileType::Other);
                DirEntry { name, file_type }
            })
            .collect())
    }

    fn read_link(&self, path: &str) -> io::Result<String> {
        let i = self.lookup(path)?;
        if i.file_type() != FileType::Symlink {
            return Err(invalid("xfs: not a symlink"));
        }
        Ok(String::from_utf8_lossy(&self.slurp(&i)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny version 2 file system: 4 KiB blocks, one allocation group of
    /// 16 blocks, 256-byte inodes in block 1, a short-form root holding a
    /// file with extent-mapped data in block 3 and a local symlink.
    fn image() -> Vec<u8> {
        let mut img = vec![0u8; 16 * 4096];
        let sb = &mut img[..512];
        sb[0..4].copy_from_slice(&SUPER_MAGIC.to_be_bytes());
        sb[4..8].copy_from_slice(&4096u32.to_be_bytes());
        sb[56..64].copy_from_slice(&16u64.to_be_bytes()); // root: block 1, slot 0
        sb[84..88].copy_from_slice(&16u32.to_be_bytes());
        sb[88..92].copy_from_slice(&1u32.to_be_bytes());
        sb[100..102].copy_from_slice(&(2u16 | VER_DIRV2).to_be_bytes());
        sb[104..106].copy_from_slice(&256u16.to_be_bytes());
        sb[106..108].copy_from_slice(&16u16.to_be_bytes());
        sb[108..112].copy_from_slice(b"test");
        sb[120] = 12;
        sb[122] = 8;
        sb[123] = 4;
        sb[124] = 4;
        let mut inode = |slot: usize, mode: u16, format: u8, size: u64, nexts: u32, fork: &[u8]| {
            let o = 4096 + slot * 256;
            img[o..o + 2].copy_from_slice(&0x494eu16.to_be_bytes());
            img[o + 2..o + 4].copy_from_slice(&mode.to_be_bytes());
            img[o + 4] = 2;
            img[o + 5] = format;
            img[o + 56..o + 64].copy_from_slice(&size.to_be_bytes());
            img[o + 76..o + 80].copy_from_slice(&nexts.to_be_bytes());
            img[o + 100..o + 100 + fork.len()].copy_from_slice(fork);
        };
        // short-form root: 2 entries, 4-byte inodes, parent 16
        let mut sf = vec![2, 0, 0, 0, 0, 16];
        for (name, ino) in [("data", 17u32), ("link", 18)] {
            sf.extend([name.len() as u8, 0, 0]);
            sf.extend(name.as_bytes());
            sf.extend(ino.to_be_bytes());
        }
        inode(0, 0x41ed, FMT_LOCAL, sf.len() as u64, 0, &sf);
        // one extent: file block 0 -> fs block 3, 1 block
        let (l0, l1) = (0u64, 3u64 << 21 | 1);
        let mut ext = l0.to_be_bytes().to_vec();
        ext.extend(l1.to_be_bytes());
        inode(1, 0x81a4, FMT_EXTENTS, 5, 1, &ext);
        inode(2, 0xa1ff, FMT_LOCAL, 4, 0, b"data");
        img[3 * 4096..3 * 4096 + 5].copy_from_slice(b"world");
        img
    }

    #[test]
    fn reads_files_dirs_and_links() {
        let fs = Xfs::open(image()).unwrap();
        assert_eq!(fs.label, "test");
        let mut names: Vec<_> = fs
            .read_dir(".")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.file_type))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            names,
            [
                ("data".into(), FileType::File),
                ("link".into(), FileType::Symlink)
            ]
        );
        let mut buf = [0; 5];
        fs.open("data").unwrap().read_exact_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"world");
        assert_eq!(fs.read_link("link").unwrap(), "data");
        assert!(fs.stat("nope").is_err());
    }

    #[test]
    fn hostile_superblocks_are_errors_not_panics() {
        let mut img = image();
        img[124] = 200; // allocation-group log far past 40
        let fs = Xfs::open(img.clone()).unwrap();
        let _ = fs.read_dir(".");
        let _ = fs.stat("data");
        img[124] = 4;
        img[56..64].fill(0); // no root inode
        assert!(Xfs::open(img.clone()).is_err());
        img[56..64].copy_from_slice(&16u64.to_be_bytes());
        img[84..88].copy_from_slice(&u32::MAX.to_be_bytes()); // huge groups
        img[88..92].copy_from_slice(&u32::MAX.to_be_bytes());
        img[124] = 32;
        let fs = Xfs::open(img).unwrap();
        let _ = fs.stat("data");
    }

    #[test]
    fn local_read_past_the_fork_is_zeros() {
        let mut img = image();
        let size = 4096 + 2 * 256 + 56;
        img[size..size + 8].copy_from_slice(&100u64.to_be_bytes());
        let fs = Xfs::open(img).unwrap();
        let f = fs.open("link").unwrap();
        let mut b = [0xffu8; 8];
        assert_eq!(f.read_at(50, &mut b).unwrap(), 8);
        assert_eq!(b, [0; 8]);
    }

    #[test]
    fn overlapping_extents_are_dropped() {
        let mut img = image();
        // two extents for the file: file blocks 0..1 -> fs block 3, then a
        // second run that starts at file block 0 again
        let o = 4096 + 256 + 100;
        let e = |off: u64, start: u64| {
            let mut v = (off << 9).to_be_bytes().to_vec();
            v.extend((start << 21 | 1).to_be_bytes());
            v
        };
        img[o..o + 16].copy_from_slice(&e(0, 3));
        img[o + 16..o + 32].copy_from_slice(&e(0, 4));
        img[4096 + 256 + 76..4096 + 256 + 80].copy_from_slice(&2u32.to_be_bytes());
        let fs = Xfs::open(img).unwrap();
        let mut b = [0u8; 5];
        fs.open("data").unwrap().read_exact_at(0, &mut b).unwrap();
        assert_eq!(&b, b"world");
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(Xfs::open(vec![0u8; 4096]).is_err());
    }
}
