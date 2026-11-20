//! Read-only SGI EFS: 512-byte basic blocks, 128-byte inodes in cylinder
//! groups, extent-mapped files. Ported from Instigator's `efs` package.

use disk_rofs::{invalid, not_found, DirEntry, FileType, ReadAt, ReadFs, Stat};
use std::io;

const SUPER_MAGIC: u32 = 0x00072959;
const SUPER_MAGIC_GROWN: u32 = 0x0007295a;
const BLOCK: u64 = 512;
const ROOT_INO: u32 = 2;
const DIRECT_EXTENTS: usize = 12;
const DIRBLK_MAGIC: u16 = 0xbeef;
/// Longest file or directory read into memory; a valid EFS file is under 2 GiB.
const MAX_SLURP: u32 = 64 << 20;

pub struct Efs<D> {
    dev: D,
    first_cg: u32,
    cg_fsize: u32,
    cg_isize: u32,
    ncg: u32,
}

#[derive(Clone, Copy)]
struct Extent {
    block: u32,
    len: u32,
    offset: u32,
}

struct Inode {
    mode: u16,
    size: u32,
    extents: Vec<Extent>,
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

fn extent(b: &[u8]) -> Extent {
    let (w1, w2) = (be32(b, 0), be32(b, 4));
    Extent {
        block: w1 & 0xff_ffff,
        len: w2 >> 24,
        offset: w2 & 0xff_ffff,
    }
}

impl<D: ReadAt + Send + Sync> Efs<D> {
    /// Opens the file system at the start of `dev`.
    pub fn open(dev: D) -> io::Result<Self> {
        let mut sb = [0u8; 512];
        dev.read_exact_at(BLOCK, &mut sb)?;
        let magic = be32(&sb, 28);
        if magic != SUPER_MAGIC && magic != SUPER_MAGIC_GROWN {
            return Err(invalid(format!("efs: bad superblock magic {magic:#x}")));
        }
        let fs = Efs {
            dev,
            first_cg: be32(&sb, 4),
            cg_fsize: be32(&sb, 8),
            cg_isize: be16(&sb, 12) as u32,
            ncg: be16(&sb, 18) as u32,
        };
        // The on-disk counts are signed 16-bit; negative is corrupt.
        if fs.first_cg == 0
            || fs.cg_fsize == 0
            || fs.cg_isize == 0
            || fs.cg_isize > 0x7fff
            || fs.ncg == 0
            || fs.ncg > 0x7fff
        {
            return Err(invalid("efs: implausible geometry"));
        }
        Ok(fs)
    }

    fn block(&self, n: u64) -> io::Result<[u8; 512]> {
        let mut b = [0; 512];
        self.dev.read_exact_at(n * BLOCK, &mut b)?;
        Ok(b)
    }

    fn inode(&self, ino: u32) -> io::Result<Inode> {
        let per_cg = self.cg_isize * 4;
        let cg = ino / per_cg;
        if cg >= self.ncg {
            return Err(invalid(format!(
                "efs: inode {ino} beyond the cylinder groups"
            )));
        }
        let bb =
            self.first_cg as u64 + cg as u64 * self.cg_fsize as u64 + ((ino % per_cg) / 4) as u64;
        let blk = self.block(bb)?;
        let d = &blk[(ino % 4) as usize * 128..][..128];
        let size = be32(d, 8);
        if size > i32::MAX as u32 {
            return Err(invalid(format!("efs: inode {ino}: negative size")));
        }
        let numext = be16(d, 28) as usize;
        if numext > 0x7fff {
            return Err(invalid(format!(
                "efs: inode {ino}: implausible extent count"
            )));
        }
        let mut extents = vec![];
        if numext <= DIRECT_EXTENTS {
            extents.extend((0..numext).map(|i| extent(&d[32 + i * 8..])));
        } else {
            // The direct slots point at blocks holding the real extent table.
            'outer: for i in 0..DIRECT_EXTENTS {
                let ie = extent(&d[32 + i * 8..]);
                if ie.len == 0 {
                    break;
                }
                for b in 0..ie.len {
                    let blk = self.block(ie.block as u64 + b as u64)?;
                    for k in 0..64 {
                        if extents.len() >= numext {
                            break 'outer;
                        }
                        extents.push(extent(&blk[k * 8..]));
                    }
                }
            }
            if extents.len() < numext {
                return Err(invalid(format!("efs: inode {ino}: extent table short")));
            }
        }
        extents.sort_by_key(|e| e.offset);
        Ok(Inode {
            mode: be16(d, 0),
            size,
            extents,
        })
    }

    fn read_inode_at(&self, ino: &Inode, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        let size = ino.size as u64;
        if pos >= size {
            return Ok(0);
        }
        let want = buf.len().min((size - pos) as usize);
        let mut done = 0;
        while done < want {
            let at = pos + done as u64;
            let (lb, bo) = ((at / BLOCK) as u32, (at % BLOCK) as usize);
            let n = (BLOCK as usize - bo).min(want - done);
            let hit = ino
                .extents
                .iter()
                .find(|e| e.offset <= lb && lb < e.offset + e.len);
            match hit {
                Some(e) => {
                    let blk = self.block(e.block as u64 + (lb - e.offset) as u64)?;
                    buf[done..done + n].copy_from_slice(&blk[bo..bo + n]);
                }
                None => buf[done..done + n].fill(0), // hole
            }
            done += n;
        }
        Ok(done)
    }

    fn slurp(&self, ino: &Inode) -> io::Result<Vec<u8>> {
        if ino.size > MAX_SLURP {
            return Err(invalid("efs: file too large to read whole"));
        }
        let mut v = vec![0; ino.size as usize];
        let n = self.read_inode_at(ino, 0, &mut v)?;
        v.truncate(n);
        Ok(v)
    }

    fn entries(&self, ino: &Inode) -> io::Result<Vec<(String, u32)>> {
        if ino.file_type() != FileType::Dir {
            return Err(invalid("efs: not a directory"));
        }
        let data = self.slurp(ino)?;
        let mut out = vec![];
        for blk in data.chunks(BLOCK as usize) {
            if blk.len() < 4 || be16(blk, 0) != DIRBLK_MAGIC {
                continue;
            }
            for s in 0..blk[3] as usize {
                let Some(&slot) = blk.get(4 + s) else { break };
                let so = (slot as usize) << 1;
                if so == 0 || so + 5 > blk.len() {
                    continue;
                }
                let nl = blk[so + 4] as usize;
                if so + 5 + nl > blk.len() {
                    continue;
                }
                let name = String::from_utf8_lossy(&blk[so + 5..so + 5 + nl]).into_owned();
                out.push((name, be32(blk, so)));
            }
        }
        Ok(out)
    }

    fn lookup(&self, path: &str) -> io::Result<Inode> {
        let mut ino = self.inode(ROOT_INO)?;
        let parts: Vec<_> = path
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .collect();
        for (i, part) in parts.iter().enumerate() {
            let next = self
                .entries(&ino)
                .map_err(|_| not_found(path))?
                .into_iter()
                .find(|(n, _)| n == part)
                .filter(|(_, ino)| *ino != 0)
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
    fs: &'a Efs<D>,
    ino: Inode,
}

impl<D: ReadAt + Send + Sync> ReadAt for File<'_, D> {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.fs.read_inode_at(&self.ino, pos, buf)
    }
}

impl<D: ReadAt + Send + Sync> ReadFs for Efs<D> {
    fn stat(&self, path: &str) -> io::Result<Stat> {
        let i = self.lookup(path)?;
        Ok(Stat {
            file_type: i.file_type(),
            size: i.size as u64,
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
        let mut out = vec![];
        for (name, ino) in self.entries(&dir)? {
            if name == "." || name == ".." {
                continue;
            }
            let file_type = self
                .inode(ino)
                .map(|i| i.file_type())
                .unwrap_or(FileType::Other);
            out.push(DirEntry { name, file_type });
        }
        Ok(out)
    }

    fn read_link(&self, path: &str) -> io::Result<String> {
        let i = self.lookup(path)?;
        if i.file_type() != FileType::Symlink {
            return Err(invalid("efs: not a symlink"));
        }
        Ok(String::from_utf8_lossy(&self.slurp(&i)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-cylinder-group image: inodes at block 2, data from block 8.
    fn image() -> Vec<u8> {
        let mut img = vec![0u8; 64 * 512];
        let put32 =
            |img: &mut Vec<u8>, o: usize, v: u32| img[o..o + 4].copy_from_slice(&v.to_be_bytes());
        let sb = 512;
        put32(&mut img, sb + 4, 2); // first cg
        put32(&mut img, sb + 8, 60); // blocks per cg
        img[sb + 12..sb + 14].copy_from_slice(&2u16.to_be_bytes()); // inode blocks
        img[sb + 18..sb + 20].copy_from_slice(&1u16.to_be_bytes()); // ncg
        put32(&mut img, sb + 28, SUPER_MAGIC);
        let inode = |img: &mut Vec<u8>, n: usize, mode: u16, size: u32, block: u32| {
            let o = 2 * 512 + n * 128;
            img[o..o + 2].copy_from_slice(&mode.to_be_bytes());
            img[o + 8..o + 12].copy_from_slice(&size.to_be_bytes());
            img[o + 28..o + 30].copy_from_slice(&1u16.to_be_bytes());
            put32(img, o + 32, block);
            put32(img, o + 36, 1 << 24);
        };
        inode(&mut img, 2, 0x41ed, 512, 8); // root
        inode(&mut img, 3, 0x81a4, 5, 9); // /hello
        inode(&mut img, 4, 0xa1ff, 3, 10); // /lnk -> abc
        let dir = 8 * 512;
        img[dir..dir + 2].copy_from_slice(&DIRBLK_MAGIC.to_be_bytes());
        img[dir + 3] = 2;
        for (slot, (so, ino, name)) in [(0x20usize, 3u32, "hello"), (0x30, 4, "lnk")]
            .into_iter()
            .enumerate()
        {
            img[dir + 4 + slot] = (so >> 1) as u8;
            put32(&mut img, dir + so, ino);
            img[dir + so + 4] = name.len() as u8;
            img[dir + so + 5..dir + so + 5 + name.len()].copy_from_slice(name.as_bytes());
        }
        img[9 * 512..9 * 512 + 5].copy_from_slice(b"world");
        img[10 * 512..10 * 512 + 3].copy_from_slice(b"abc");
        img
    }

    #[test]
    fn reads_files_dirs_and_links() {
        let fs = Efs::open(image()).unwrap();
        let names: Vec<_> = fs
            .read_dir(".")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.file_type))
            .collect();
        assert_eq!(
            names,
            [
                ("hello".into(), FileType::File),
                ("lnk".into(), FileType::Symlink)
            ]
        );
        let mut buf = [0; 5];
        fs.open("hello")
            .unwrap()
            .read_exact_at(0, &mut buf)
            .unwrap();
        assert_eq!(&buf, b"world");
        assert_eq!(fs.read_link("lnk").unwrap(), "abc");
        assert!(fs.stat("missing").is_err());
    }

    #[test]
    fn hostile_inodes_and_geometry_are_errors() {
        let mut img = image();
        // zero-ino directory entry is "not found", not a read of inode 0
        let dir = 8 * 512;
        img[dir + 0x20..dir + 0x24].copy_from_slice(&0u32.to_be_bytes());
        let fs = Efs::open(img.clone()).unwrap();
        assert!(fs.stat("hello").is_err());
        // extent count over the signed 16-bit limit
        let o = 2 * 512 + 3 * 128;
        img[o + 28..o + 30].copy_from_slice(&0xffffu16.to_be_bytes());
        let fs = Efs::open(img.clone()).unwrap();
        assert!(fs.inode(3).is_err());
        // an indirect table whose first direct extent is empty is short, not a read of block 0
        img[o + 28..o + 30].copy_from_slice(&13u16.to_be_bytes());
        img[o + 32..o + 40].fill(0);
        assert!(fs.inode(3).is_err());
        // negative cylinder-group count
        img[512 + 18..512 + 20].copy_from_slice(&0x8000u16.to_be_bytes());
        assert!(Efs::open(img).is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(Efs::open(vec![0u8; 4096]).is_err());
    }
}
