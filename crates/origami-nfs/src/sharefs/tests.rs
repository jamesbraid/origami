use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use super::table::PathTable;
use super::*;
use crate::testutil::TempDir;

/// Makes `base/share` holding one file and `base/outside`, which nothing
/// the share does may touch.
fn new_share(opts: Options) -> (Share, PathBuf, TempDir) {
    let base = TempDir::new();
    let share = base.path().join("share");
    let outside = base.path().join("outside");
    for d in [share.clone(), outside.join("inner")] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(outside.join("secret"), "secret").unwrap();
    fs::write(share.join("f"), "data").unwrap();
    let mut opts = opts;
    if opts.path_table.is_none() && !INODES_USABLE {
        opts.path_table = Some(base.path().join("handles"));
    }
    let f = Share::open("s", &share, opts).unwrap();
    (f, outside, base)
}

fn root(f: &Share) -> Node {
    f.mount_root(b"/s").unwrap()
}

fn lookup(f: &Share, dir: &Node, name: &str) -> Node {
    f.lookup(dir, name.as_bytes())
        .unwrap_or_else(|e| panic!("lookup({name:?}): {e}"))
}

fn want_err<T: std::fmt::Debug>(what: &str, got: Result<T>, want: Error) {
    assert_eq!(got.as_ref().err(), Some(&want), "{what}: {got:?}");
}

/// Fails if anything was created in or changed under the outside
/// directory.
fn outside_untouched(outside: &std::path::Path) {
    let n = fs::read_dir(outside).unwrap().count();
    assert_eq!(n, 2, "outside holds {n} entries, want inner and secret");
    assert_eq!(
        fs::read_dir(outside.join("inner")).unwrap().count(),
        0,
        "outside/inner gained an entry"
    );
    assert_eq!(
        fs::read_to_string(outside.join("secret")).unwrap(),
        "secret"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = fs::metadata(outside.join("secret"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(m, 0o644, "outside/secret mode is {m:o}");
    }
}

fn unset() -> SetAttr {
    SetAttr::default()
}

#[test]
fn names_cannot_leave_the_share() {
    let (f, outside, _g) = new_share(Options::default());
    let r = root(&f);

    assert_eq!(
        lookup(&f, &r, "..").path,
        b".",
        "\"..\" of the root is the root"
    );
    for name in ["../outside", "../outside/secret", "a/b", ""] {
        want_err(
            &format!("lookup {name:?}"),
            f.lookup(&r, name.as_bytes()),
            Error::NotFound,
        );
    }
    for name in ["..", ".", "../escape", "a/b", ""] {
        let n = name.as_bytes();
        want_err(
            &format!("create {name:?}"),
            f.create(&r, n, CreateHow::Unchecked, &unset(), [0; 8]),
            Error::Inval,
        );
        want_err(
            &format!("mkdir {name:?}"),
            f.mkdir(&r, n, &unset()),
            Error::Inval,
        );
        want_err(
            &format!("rename to {name:?}"),
            f.rename(&r, b"f", &r, n),
            Error::Inval,
        );
        want_err(&format!("remove {name:?}"), f.remove(&r, n), Error::Inval);
    }
    assert!(
        f.mount_root(b"/s/../outside").is_err(),
        "mount through .. succeeded"
    );
    want_err(
        "mount of another name",
        f.mount_root(b"/other"),
        Error::NotFound,
    );
    outside_untouched(&outside);
}

// A symlink in the share that leads out is served as a link; nothing the
// server does follows it.
#[cfg(unix)]
#[test]
fn symlink_out_is_never_followed() {
    use std::os::unix::fs::symlink;
    let (f, outside, _g) = new_share(Options::default());
    let share = f.dir().to_path_buf();
    symlink(&outside, share.join("dir")).unwrap();
    symlink(outside.join("secret"), share.join("file")).unwrap();
    let r = root(&f);
    let dir = lookup(&f, &r, "dir");
    let file = lookup(&f, &r, "file");
    assert!(dir.mode() & 0o170000 == 0o120000 && file.mode() & 0o170000 == 0o120000);
    assert_eq!(
        f.readlink(&dir).unwrap(),
        outside.to_str().unwrap().as_bytes()
    );

    want_err(
        "lookup through a link",
        f.lookup(&dir, b"secret"),
        Error::NotDir,
    );
    want_err(
        "create through a link",
        f.create(&dir, b"x", CreateHow::Unchecked, &unset(), [0; 8]),
        Error::NotDir,
    );
    want_err("read_dir of a link", f.read_dir(&dir), Error::NotDir);
    want_err(
        "read_at of a link",
        f.read_at(&file, &mut [0; 8], 0),
        Error::Inval,
    );
    want_err(
        "write_at to a link",
        f.write_at(&file, b"x", 0),
        Error::Inval,
    );
    let trunc = SetAttr {
        size: Some(0),
        ..unset()
    };
    want_err("truncating a link", f.set_attr(&file, &trunc), Error::Inval);
    let chmod = SetAttr {
        mode: Some(0o777),
        ..unset()
    };
    f.set_attr(&file, &chmod).expect("chmod of a link");
    want_err("rmdir of a link", f.rmdir(&r, b"dir"), Error::NotDir);
    f.remove(&r, b"file").expect("remove of a link");
    outside_untouched(&outside);
}

// A directory the client holds a handle under, replaced on the host by a
// link out of the share, still can't be written through.
#[cfg(unix)]
#[test]
fn swapped_directory_cannot_escape() {
    use std::os::unix::fs::symlink;
    for table in [false, true] {
        let base = TempDir::new();
        let opts = Options {
            path_table: table.then(|| base.path().join("handles")),
            ..Options::default()
        };
        let (f, outside, _g) = new_share(opts);
        let r = root(&f);
        let sub = f.mkdir(&r, b"sub", &unset()).unwrap();
        let inner = f.mkdir(&sub, b"inner", &unset()).unwrap();
        let g = f
            .create(&inner, b"g", CreateHow::Unchecked, &unset(), [0; 8])
            .unwrap();
        let share = f.dir().to_path_buf();
        fs::rename(share.join("sub"), share.join("moved")).unwrap();
        symlink(&outside, share.join("sub")).unwrap();
        assert!(
            f.create(&inner, b"x", CreateHow::Unchecked, &unset(), [0; 8])
                .is_err(),
            "create under a swapped directory"
        );
        assert!(
            f.mkdir(&inner, b"x", &unset()).is_err(),
            "mkdir under a swapped directory"
        );
        let _ = f.write_at(&g, b"x", 0);
        let _ = f.rename(&inner, b"g", &r, b"stolen");
        outside_untouched(&outside);
    }
}

#[cfg(unix)]
#[test]
fn guest_symlinks_stay_text() {
    let (f, outside, _g) = new_share(Options::default());
    let r = root(&f);
    for (name, target) in [
        ("abs", "/etc"),
        ("up", "../outside"),
        ("deep", "../../../../outside/secret"),
    ] {
        let n = f
            .symlink(&r, name.as_bytes(), target.as_bytes(), &unset())
            .unwrap();
        assert_eq!(f.readlink(&n).unwrap(), target.as_bytes());
        want_err(
            &format!("create in {name}"),
            f.create(&n, b"x", CreateHow::Unchecked, &unset(), [0; 8]),
            Error::NotDir,
        );
        want_err(
            &format!("write_at {name}"),
            f.write_at(&n, b"x", 0),
            Error::Inval,
        );
    }
    want_err(
        "empty target",
        f.symlink(&r, b"empty", b"", &unset()),
        Error::Inval,
    );
    outside_untouched(&outside);
}

#[cfg(unix)]
fn host_mode(f: &Share, name: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(f.dir().join(name))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

#[cfg(unix)]
#[test]
fn setuid_is_stripped() {
    use std::os::unix::fs::PermissionsExt;
    let (f, _o, _g) = new_share(Options::default());
    let r = root(&f);
    let asked = SetAttr {
        mode: Some(0o4755),
        ..unset()
    };
    let n = f
        .create(&r, b"su", CreateHow::Guarded, &asked, [0; 8])
        .unwrap();
    assert_eq!(host_mode(&f, "su"), 0o755, "created with setuid asked");
    f.set_attr(&n, &asked).unwrap();
    assert_eq!(host_mode(&f, "su"), 0o755, "chmod 4755");

    // One set on the host is hidden, then cleared by a write, with setgid
    // on a group-executable file.
    fs::set_permissions(f.dir().join("su"), fs::Permissions::from_mode(0o6755)).unwrap();
    let n = lookup(&f, &r, "su");
    assert!(
        n.mode() & 0o4000 == 0 && n.mode() & 0o2000 != 0,
        "served mode {:o}, want setgid shown and setuid hidden",
        n.mode()
    );
    f.write_at(&n, b"x", 0).unwrap();
    assert_eq!(host_mode(&f, "su"), 0o755, "after a write");

    // A setgid directory keeps it: that bit means group inheritance.
    let dmode = SetAttr {
        mode: Some(0o2775),
        ..unset()
    };
    f.mkdir(&r, b"d", &dmode).unwrap();
    assert_eq!(host_mode(&f, "d"), 0o2775, "setgid directory");
}

// The mode a client asks for is the mode the file gets, whatever the
// server's umask.
#[cfg(unix)]
#[test]
fn create_mode_ignores_umask() {
    let (f, _o, _g) = new_share(Options::default());
    let m = SetAttr {
        mode: Some(0o666),
        ..unset()
    };
    f.create(&root(&f), b"g", CreateHow::Guarded, &m, [0; 8])
        .unwrap();
    assert_eq!(host_mode(&f, "g"), 0o666);
}

fn path_of(f: &Share, fh: &[u8]) -> Vec<u8> {
    f.node_by_handle(fh)
        .unwrap_or_else(|e| panic!("node_by_handle: {e}"))
        .path
}

fn handle_of(f: &Share, n: &Node) -> Vec<u8> {
    let fh = f.handle(n);
    assert_eq!(fh.len(), 32);
    fh
}

#[test]
fn handles_survive_restart() {
    let base = TempDir::new();
    let mut cases: Vec<(&str, Option<PathBuf>)> =
        vec![("path table", Some(base.path().join("handles")))];
    if INODES_USABLE {
        cases.push(("inodes", None));
    }
    for (name, table) in cases {
        let opts = || Options {
            path_table: table.clone(),
            ..Options::default()
        };
        let dir = base.path().join(name);
        fs::create_dir_all(dir.join("a")).unwrap();
        let f = Share::open("s", &dir, opts()).unwrap();
        let r = root(&f);
        let a = lookup(&f, &r, "a");
        let b = f
            .create(&a, b"b", CreateHow::Guarded, &unset(), [0; 8])
            .unwrap();
        let (hb, ha) = (handle_of(&f, &b), handle_of(&f, &a));
        f.rename(&r, b"a", &r, b"c").unwrap();
        assert_eq!(path_of(&f, &hb), b"c/b", "{name}: after rename");
        drop(f);

        let f = Share::open("s", &dir, opts()).unwrap();
        assert_eq!(path_of(&f, &hb), b"c/b", "{name}: after restart");
        assert_eq!(path_of(&f, &ha), b"c", "{name}: directory after restart");
        let c = lookup(&f, &root(&f), "c");
        f.remove(&c, b"b").unwrap();
        want_err(
            &format!("{name}: handle of a removed file"),
            f.node_by_handle(&hb),
            Error::Stale,
        );
    }
}

// With inodes, a handle finds its file after the host moves it, and goes
// stale when the host replaces it.
#[test]
fn inode_handles_follow_the_host() {
    if !INODES_USABLE {
        return;
    }
    let (f, _o, _g) = new_share(Options::default());
    let r = root(&f);
    let fh = handle_of(&f, &lookup(&f, &r, "f"));
    let share = f.dir().to_path_buf();
    fs::create_dir(share.join("d")).unwrap();
    fs::rename(share.join("f"), share.join("d/f2")).unwrap();
    assert_eq!(path_of(&f, &fh), b"d/f2", "after a host rename");
    fs::remove_file(share.join("d/f2")).unwrap();
    // Birth times come from the kernel's coarse clock; a file replaced
    // within one tick of its predecessor can't be told apart from it.
    std::thread::sleep(Duration::from_millis(50));
    fs::write(share.join("d/f2"), "").unwrap();
    match f.node_by_handle(&fh) {
        Ok(n) => {
            // The new file reused the inode; only the generation can tell.
            if n.key.generation == 0 {
                eprintln!("inode reused and this filesystem reports no birth time");
                return;
            }
            panic!(
                "handle of a replaced file resolved to {:?}",
                String::from_utf8_lossy(&n.path)
            );
        }
        Err(e) => assert_eq!(e, Error::Stale, "replaced file"),
    }
}

#[test]
fn bad_handles() {
    let (f, _o, g) = new_share(Options::default());
    let fh = handle_of(&f, &root(&f));
    want_err(
        "short handle",
        f.node_by_handle(&fh[..16]),
        Error::BadHandle,
    );
    let mut bad = fh.clone();
    bad[0] = 9;
    want_err("unknown format", f.node_by_handle(&bad), Error::BadHandle);
    let mut other = fh.clone();
    other[7] ^= 1;
    want_err(
        "another share's handle",
        f.node_by_handle(&other),
        Error::Stale,
    );

    let tab = Share::open(
        "s",
        f.dir(),
        Options {
            path_table: Some(g.path().join("table-handles")),
            ..Options::default()
        },
    )
    .unwrap();
    if INODES_USABLE {
        want_err(
            "an inode handle given to a path-table share",
            tab.node_by_handle(&fh),
            Error::Stale,
        );
    }
}

#[test]
fn exclusive_create() {
    let (f, _o, _g) = new_share(Options::default());
    let r = root(&f);
    let mut verf = [0, 0, 0, 1, 0, 0, 0, 2];
    let n = f
        .create(&r, b"x", CreateHow::Exclusive, &unset(), verf)
        .unwrap();
    let again = f
        .create(&r, b"x", CreateHow::Exclusive, &unset(), verf)
        .expect("retransmission");
    assert_eq!(again.path, n.path, "retransmission found another file");
    verf[7] += 1;
    want_err(
        "another verifier",
        f.create(&r, b"x", CreateHow::Exclusive, &unset(), verf),
        Error::Exist,
    );
    want_err(
        "guarded",
        f.create(&r, b"x", CreateHow::Guarded, &unset(), verf),
        Error::Exist,
    );
}

// A READDIR cookie stays with its name, so a client listing a directory
// while removing what it has seen loses nothing.
#[test]
fn read_dir_cookies_survive_removal() {
    let (f, _o, _g) = new_share(Options::default());
    let r = root(&f);
    for name in ["a", "b", "c", "d", "e"] {
        f.create(&r, name.as_bytes(), CreateHow::Guarded, &unset(), [0; 8])
            .unwrap();
    }
    let before = f.read_dir(&r).unwrap();
    let mut cookies = std::collections::HashMap::new();
    for (i, e) in before.iter().enumerate() {
        cookies.insert(e.name.clone(), e.cookie);
        assert!(
            i == 0 || e.cookie > before[i - 1].cookie,
            "entries not in cookie order"
        );
    }
    for e in &before[..3] {
        f.remove(&r, &e.name).unwrap();
    }
    let after = f.read_dir(&r).unwrap();
    assert_eq!(after.len(), before.len() - 3);
    for e in &after {
        assert_eq!(cookies[&e.name], e.cookie, "{:?} moved cookie", e.name);
    }
}

#[test]
fn path_table_file() {
    let base = TempDir::new();
    let file = base.path().join("sub").join("handles");
    let t = PathTable::open(&file).unwrap();
    let a = t.id_for(b"a").unwrap();
    let b = t.id_for(b"a/b").unwrap();
    let c = t.id_for(b"c d\n").unwrap();
    t.rename(b"a", b"x");
    t.remove(b"c d\n");
    drop(t);

    // The file opens with Go's next line, then changes are appended as set
    // ID "PATH" and del ID lines.
    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(
        text,
        "next 1\nset 1 \"a\"\nset 2 \"a/b\"\nset 3 \"c d\\n\"\nset 1 \"x\"\nset 2 \"x/b\"\ndel 3\n"
    );

    let t = PathTable::open(&file).unwrap();
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "next 4\nset 1 \"x\"\nset 2 \"x/b\"\n",
        "compacted on open"
    );
    assert_eq!(t.path(a).as_deref(), Some(&b"x"[..]));
    assert_eq!(t.path(b).as_deref(), Some(&b"x/b"[..]));
    assert!(t.path(c).is_none(), "removed path survived");
    let d = t.id_for(b"new").unwrap();
    assert!(d > c, "new id {d} reuses one at or below {c}");
}

// The first file a Go server wrote is one this server reads: ids keep
// their numbers, quoting included.
#[test]
fn path_table_reads_a_go_file() {
    let base = TempDir::new();
    let file = base.path().join("handles");
    fs::write(
        &file,
        "set 1 \".\"\nset 2 \"caf\\u00e9\"\nset 3 \"a\\tb\"\ndel 1\nset 7 \"\\xff\"\n",
    )
    .unwrap();
    let t = PathTable::open(&file).unwrap();
    assert!(t.path(1).is_none());
    assert_eq!(t.path(2).unwrap(), "caf\u{e9}".as_bytes());
    assert_eq!(t.path(3).unwrap(), b"a\tb");
    assert_eq!(t.path(7).unwrap(), b"\xff");
    assert_eq!(t.id_for(b"fresh").unwrap(), 8);
}

// On a filesystem without birth times (an NFS mount) the host gives a
// recreated file the inode of the one before it. Without a generation to
// tell them apart the share numbers paths itself, and a name recreated
// gets a handle the old one cannot reach.
#[test]
fn a_recreated_name_gets_a_new_handle_without_birth_time() {
    let base = TempDir::new();
    let dir = base.path().join("share");
    fs::create_dir_all(&dir).unwrap();
    let opts = Options {
        no_birth_time: true,
        table_fallback: Some(base.path().join("handles")),
        ..Options::default()
    };
    let f = Share::open("s", &dir, opts).unwrap();
    let r = root(&f);
    let make = |name: &[u8]| {
        f.create(&r, name, CreateHow::Guarded, &unset(), [0; 8])
            .unwrap()
    };
    for how in ["remove", "rename over"] {
        let old = handle_of(&f, &make(b"t"));
        if how == "remove" {
            f.remove(&r, b"t").unwrap();
        } else {
            make(b"u");
            f.rename(&r, b"u", &r, b"t").unwrap();
            f.remove(&r, b"t").unwrap();
        }
        let again = handle_of(&f, &make(b"t"));
        assert_ne!(again, old, "{how}: the recreated file has the old handle");
        want_err(how, f.node_by_handle(&old), Error::Stale);
        f.remove(&r, b"t").unwrap();
    }
}

// A handle to a removed file stays dead across any number of restarts,
// even when it had the highest id the table ever gave out.
#[test]
fn a_removed_handle_stays_dead_across_restarts() {
    let base = TempDir::new();
    let dir = base.path().join("share");
    fs::create_dir_all(&dir).unwrap();
    let open = || {
        let opts = Options {
            no_birth_time: true,
            table_fallback: Some(base.path().join("handles")),
            ..Options::default()
        };
        Share::open("s", &dir, opts).unwrap()
    };
    let f = open();
    let n = f
        .create(&root(&f), b"t", CreateHow::Guarded, &unset(), [0; 8])
        .unwrap();
    let old = handle_of(&f, &n);
    f.remove(&root(&f), b"t").unwrap();
    drop(f);
    // The first restart replays the removal, the second reads a compacted
    // table that no longer lists the id.
    for _ in 0..2 {
        let f = open();
        want_err(
            "old handle after restart",
            f.node_by_handle(&old),
            Error::Stale,
        );
        let n2 = f
            .create(&root(&f), b"t", CreateHow::Guarded, &unset(), [0; 8])
            .unwrap();
        assert_ne!(
            handle_of(&f, &n2),
            old,
            "a new file reuses the removed handle"
        );
        want_err(
            "old handle after a create",
            f.node_by_handle(&old),
            Error::Stale,
        );
        f.remove(&root(&f), b"t").unwrap();
    }
}

#[test]
fn no_birth_time_needs_a_table() {
    let base = TempDir::new();
    let dir = base.path().join("share");
    fs::create_dir_all(&dir).unwrap();
    let bare = Options {
        no_birth_time: true,
        ..Options::default()
    };
    assert!(Share::open("s", &dir, bare).is_err());
    // An explicit table wins over the fallback.
    let explicit = base.path().join("explicit");
    let opts = Options {
        no_birth_time: true,
        path_table: Some(explicit.clone()),
        table_fallback: Some(base.path().join("other")),
        ..Options::default()
    };
    Share::open("s", &dir, opts).unwrap();
    assert!(explicit.exists());
    assert!(!base.path().join("other").exists());
}

// A share that falls back keeps the documented path-table layout.
#[test]
fn the_fallback_handle_is_a_path_table_handle() {
    let opts = Options {
        no_birth_time: true,
        table_fallback: Some(TempDir::new().path().join("handles")),
        ..Options::default()
    };
    let (f, _outside, _g) = new_share(opts);
    let h = handle_of(&f, &lookup(&f, &root(&f), "f"));
    assert_eq!((h[0], h[1] | h[2] | h[3]), (FMT_TABLE, 0));
    assert!(h[8..16].iter().chain(&h[24..]).all(|&b| b == 0), "{h:02x?}");
}

// A table written before the next line existed still reads.
#[test]
fn a_table_with_a_next_line_never_reissues_a_freed_id() {
    let base = TempDir::new();
    let file = base.path().join("handles");
    fs::write(&file, "next 9\nset 2 \"a\"\n").unwrap();
    let t = PathTable::open(&file).unwrap();
    assert_eq!(t.id_for(b"fresh").unwrap(), 9);
}
