//! Several shares behind one server, each mounted as `/name`.

use crate::fs::{CreateHow, DirEntry, Error, FileSystem, Node, Result, SetAttr, Space};
use crate::sharefs::Share;

/// A set of [`Share`]s served as one filesystem. A mount path picks the
/// share by its first component; every node and handle after that belongs
/// to the share that made it.
pub struct Shares {
    shares: Vec<Share>,
}

impl Shares {
    pub fn new(shares: Vec<Share>) -> Shares {
        Shares { shares }
    }

    fn of(&self, n: &Node) -> Result<&Share> {
        self.shares.get(n.owner).ok_or(Error::Stale)
    }

    fn tag(&self, owner: usize, mut n: Node) -> Node {
        n.owner = owner;
        n
    }
}

impl FileSystem for Shares {
    fn mount_root(&self, path: &[u8]) -> Result<Node> {
        let mut last = Error::NotFound;
        for (i, s) in self.shares.iter().enumerate() {
            match s.mount_root(path) {
                Ok(n) => return Ok(self.tag(i, n)),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn node_by_id(&self, _id: u64) -> Result<Node> {
        Err(Error::Stale)
    }

    fn lookup(&self, dir: &Node, name: &[u8]) -> Result<Node> {
        Ok(self.tag(dir.owner, self.of(dir)?.lookup(dir, name)?))
    }

    fn read_dir(&self, dir: &Node) -> Result<Vec<DirEntry>> {
        self.of(dir)?.read_dir(dir)
    }

    fn read_at(&self, n: &Node, buf: &mut [u8], off: u64) -> Result<usize> {
        self.of(n)?.read_at(n, buf, off)
    }

    fn readlink(&self, n: &Node) -> Result<Vec<u8>> {
        self.of(n)?.readlink(n)
    }

    fn exports(&self) -> Vec<Vec<u8>> {
        self.shares.iter().flat_map(|s| s.exports()).collect()
    }

    fn handle(&self, n: &Node) -> Vec<u8> {
        match self.of(n) {
            Ok(s) => s.handle(n),
            Err(_) => vec![0; 32],
        }
    }

    fn node_by_handle(&self, fh: &[u8]) -> Result<Node> {
        let mut last = Error::BadHandle;
        for (i, s) in self.shares.iter().enumerate() {
            match s.node_by_handle(fh) {
                Ok(n) => return Ok(self.tag(i, n)),
                // A handle carries its share's id, so the other shares
                // call it stale; a share that finds the file gone says so
                // too, and wins.
                Err(Error::Stale) if matches!(last, Error::BadHandle) => last = Error::Stale,
                Err(Error::BadHandle) => {}
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn space(&self) -> Result<Space> {
        // The server asks for a share's capacity with no node in hand.
        self.shares
            .first()
            .map_or(Ok(Space::default()), |s| s.space())
    }

    fn writable(&self) -> bool {
        self.shares.iter().any(|s| s.writable())
    }

    fn set_attr(&self, n: &Node, a: &SetAttr) -> Result<()> {
        self.of(n)?.set_attr(n, a)
    }

    fn write_at(&self, n: &Node, data: &[u8], off: u64) -> Result<()> {
        self.of(n)?.write_at(n, data, off)
    }

    fn create(
        &self,
        dir: &Node,
        name: &[u8],
        how: CreateHow,
        a: &SetAttr,
        verf: [u8; 8],
    ) -> Result<Node> {
        Ok(self.tag(dir.owner, self.of(dir)?.create(dir, name, how, a, verf)?))
    }

    fn mkdir(&self, dir: &Node, name: &[u8], a: &SetAttr) -> Result<Node> {
        Ok(self.tag(dir.owner, self.of(dir)?.mkdir(dir, name, a)?))
    }

    fn symlink(&self, dir: &Node, name: &[u8], target: &[u8], a: &SetAttr) -> Result<Node> {
        Ok(self.tag(dir.owner, self.of(dir)?.symlink(dir, name, target, a)?))
    }

    fn remove(&self, dir: &Node, name: &[u8]) -> Result<()> {
        self.of(dir)?.remove(dir, name)
    }

    fn rmdir(&self, dir: &Node, name: &[u8]) -> Result<()> {
        self.of(dir)?.rmdir(dir, name)
    }

    fn rename(&self, from_dir: &Node, from: &[u8], to_dir: &Node, to: &[u8]) -> Result<()> {
        if from_dir.owner != to_dir.owner {
            return Err(Error::XDev);
        }
        self.of(from_dir)?.rename(from_dir, from, to_dir, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharefs::Options;
    use crate::testutil::TempDir;

    #[test]
    fn mounts_and_handles_follow_the_share() {
        let base = TempDir::new();
        let (a, b) = (base.path().join("a"), base.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("one"), b"1").unwrap();
        std::fs::write(b.join("two"), b"22").unwrap();
        let open = |n: &str, d: &std::path::Path| {
            let table = base.path().join(format!("{n}.handles"));
            let opts = Options {
                path_table: Some(table),
                ..Options::default()
            };
            Share::open(n, d, opts).unwrap()
        };
        let s = Shares::new(vec![open("a", &a), open("b", &b)]);
        assert_eq!(s.exports(), vec![b"/a".to_vec(), b"/b".to_vec()]);
        assert!(matches!(s.mount_root(b"/c"), Err(Error::NotFound)));
        let root = s.mount_root(b"/b").unwrap();
        let two = s.lookup(&root, b"two").unwrap();
        assert_eq!(two.size(), 2);
        assert!(matches!(s.lookup(&root, b"one"), Err(Error::NotFound)));
        let fh = s.handle(&two);
        let again = s.node_by_handle(&fh).unwrap();
        assert_eq!((again.owner, again.size()), (1, 2));
        let mut buf = [0u8; 4];
        assert_eq!(s.read_at(&again, &mut buf, 0).unwrap(), 2);
        // The other share's file is not reachable through this handle.
        let one = s.lookup(&s.mount_root(b"/a").unwrap(), b"one").unwrap();
        assert_eq!(s.node_by_handle(&s.handle(&one)).unwrap().owner, 0);
        assert!(matches!(
            s.node_by_handle(&[0u8; 32]),
            Err(Error::BadHandle | Error::Stale)
        ));
    }
}
