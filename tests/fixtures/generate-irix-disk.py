#!/usr/bin/env python3
"""Write disk-irix-6.5.30.img, a 36 KB raw disk the app self-tests import.

    tests/fixtures/generate-irix-disk.py > tests/fixtures/machines/disk-irix-6.5.30.img

An SGI volume header whose slot 0 is a one-file EFS: /unix holds the strings
disk-detect reads a release from. It is not a bootable disk.
"""
import struct
import sys

BLOCKS = 64
disk = bytearray((8 + BLOCKS) * 512)
disk[0:4] = struct.pack(">I", 0x0BE5A941)
# Partition slot 0: size in blocks, first block, type 7 (EFS).
disk[312:324] = struct.pack(">III", BLOCKS, 8, 7)

fs = memoryview(disk)[8 * 512:]
put = lambda o, fmt, *v: struct.pack_into(">" + fmt, fs, o, *v)
sb = 512
put(sb + 4, "I", 2)        # first cylinder group
put(sb + 8, "I", 60)       # blocks per group
put(sb + 12, "H", 2)       # inode blocks
put(sb + 18, "H", 1)       # groups
put(sb + 28, "I", 0x72959)  # EFS magic


def inode(n, mode, size, block):
    o = 2 * 512 + n * 128
    put(o, "H", mode)
    put(o + 8, "I", size)
    put(o + 28, "H", 1)
    put(o + 32, "II", block, 1 << 24)


kernel = b"IRIX Release 6.5 IP30\0\0" + b"6.5\0\0" + b"6.5.30\0\0"
inode(2, 0x41ED, 512, 8)
inode(3, 0x81A4, len(kernel), 9)
d = 8 * 512
put(d, "H", 0xBEEF)
fs[d + 3] = 1
fs[d + 4] = 0x20 >> 1
put(d + 0x20, "I", 3)
fs[d + 0x24] = 4
fs[d + 0x25:d + 0x29] = b"unix"
fs[9 * 512:9 * 512 + len(kernel)] = kernel
sys.stdout.buffer.write(disk)
