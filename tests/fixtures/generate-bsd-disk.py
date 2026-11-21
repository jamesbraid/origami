#!/usr/bin/env python3
"""Write a tiny synthetic BSD disk image to stdout.

    tests/fixtures/generate-bsd-disk.py openbsd-sgi > tests/fixtures/machines/disk-openbsd-6.9.img
    tests/fixtures/generate-bsd-disk.py netbsd-label | gzip -9n > crates/disk/detect/tests/data/netbsd-9.3.img.gz
    tests/fixtures/generate-bsd-disk.py bare-ffs | gzip -9n > crates/disk/detect/tests/data/bare-openbsd.img.gz

openbsd-sgi: an SGI volume header whose slot 0 is a BSD partition holding a
disklabel at its sector 1 and a big-endian UFS1 file system with /bsd.
netbsd-label: no partition table but a little-endian disklabel at sector 1,
and a little-endian UFS2 file system with /netbsd at its partition 'a'.
bare-ffs: a little-endian UFS1 file system at byte 0 with /bsd and nothing else.
The file systems hold only a kernel version string; they are not bootable.
"""
import struct
import sys

BSIZE, FSIZE = 4096, 512
FRAG = BSIZE // FSIZE


def make_ffs(ufs2, big, name, content):
    """One cylinder group: superblock, one block of inodes, a root directory, one file."""
    e = ">" if big else "<"
    sb_at = 65536 if ufs2 else 8192
    iblkno = sb_at // FSIZE + 16  # fragments, after the superblock
    isz = 256 if ufs2 else 128
    inopb = BSIZE // isz
    fpg = iblkno + 3 * FRAG
    fs = bytearray(fpg * FSIZE)

    def put(o, fmt, *v):
        struct.pack_into(e + fmt, fs, o, *v)

    sb = sb_at
    for off, v in [(16, iblkno), (28, 0xFFFFFFFF), (44, 1), (48, BSIZE), (52, FSIZE),
                   (56, FRAG), (120, inopb), (184, inopb), (188, fpg),
                   (1320, 120 if ufs2 else 60), (1324, 2)]:
        put(sb + off, "I", v)
    if ufs2:
        put(sb + 1080, "Q", fpg)
    else:
        put(sb + 36, "I", fpg)
    put(sb + 1372, "I", 0x19540119 if ufs2 else 0x011954)

    table = iblkno * FSIZE

    def inode(n, mode, size, frag):
        o = table + n * isz
        put(o, "H", mode)
        if ufs2:
            put(o + 16, "Q", size)
            put(o + 112, "Q", frag)
        else:
            put(o + 8, "Q", size)
            put(o + 40, "I", frag)

    root, data = iblkno + FRAG, iblkno + 2 * FRAG
    inode(2, 0o40755, 512, root)
    inode(3, 0o100755, len(content), data)
    d = root * FSIZE
    p = 0
    ents = [(".", 2), ("..", 2), (name, 3)]
    for k, (n, ino) in enumerate(ents):
        ln = (8 + len(n) + 1 + 3) & ~3
        if k == len(ents) - 1:
            ln = 512 - p
        put(d + p, "I", ino)
        put(d + p + 4, "H", ln)
        fs[d + p + 6] = 4  # DT_DIR-style type byte
        fs[d + p + 7] = len(n)
        fs[d + p + 8:d + p + 8 + len(n)] = n.encode()
        p += ln
    fs[data * FSIZE:data * FSIZE + len(content)] = content
    return bytes(fs)


def disklabel(big, parts, openbsd=False):
    """A BSD disklabel of (offset, size, fstype) slots in sectors, as one 512-byte sector."""
    e = ">" if big else "<"
    magic = 0x82564557
    hdr = struct.pack(e + "IHH16s16sIIIIII", magic, 4, 0, b"synthetic", b"fixture", 512, 32, 64,
                      1000, 2048, 1 << 20)
    hdr += bytes(132 - len(hdr)) + struct.pack(e + "IHHII", magic, 0, len(parts), 8192, 8192)
    body = b"".join(struct.pack(e + "IIIBBH", size, off, 0, fstype, 0, 0) for off, size, fstype in parts)
    lab = bytearray(hdr + body)
    if openbsd:
        lab[114:116] = struct.pack(e + "H", 1)  # d_version
    lab = bytes(lab)
    x = 0
    for (w,) in struct.iter_unpack(e + "H", lab):
        x ^= w
    lab = lab[:136] + struct.pack(e + "H", x) + lab[138:]
    return lab.ljust(512, b"\0")


OPENBSD = b"\0\0OpenBSD maintainers\0OpenBSD 6.9 (GENERIC) #1: Sat Apr  3 12:00:00 MDT 2021\n\0"
NETBSD = b"\0NetBSD 9.3 (GENERIC) #0: Sun Aug  7 10:00:00 UTC 2022\n\0"

which = sys.argv[1] if len(sys.argv) > 1 else ""
if which == "openbsd-sgi":
    fs = make_ffs(False, True, "bsd", OPENBSD)
    first = 8  # sectors
    disk = bytearray(first * 512 + len(fs))
    disk[0:4] = struct.pack(">I", 0x0BE5A941)
    disk[312:324] = struct.pack(">III", len(fs) // 512, first, 4)  # slot 0: BSD
    disk[312 + 10 * 12:324 + 10 * 12] = struct.pack(">III", first + len(fs) // 512, 0, 6)  # whole disk
    disk[first * 512:] = fs
    disk[first * 512 + 512:first * 512 + 1024] = disklabel(True, [(first, len(fs) // 512, 7)], openbsd=True)
    sys.stdout.buffer.write(disk)
elif which == "netbsd-label":
    fs = make_ffs(True, False, "netbsd", NETBSD)
    first = 64
    disk = bytearray(first * 512 + len(fs))
    disk[512:1024] = disklabel(False, [(first, len(fs) // 512, 7), (0, first + len(fs) // 512, 0)])
    disk[first * 512:] = fs
    sys.stdout.buffer.write(disk)
elif which == "bare-ffs":
    sys.stdout.buffer.write(make_ffs(False, False, "bsd", OPENBSD))
else:
    sys.exit("usage: generate-bsd-disk.py openbsd-sgi | netbsd-label | bare-ffs")
