#!/bin/sh
# Finish a RAD4 install against the mounted IRIX target root.
set -eu

case "${1-}" in
    /) root= ;;
    /root) root=/root ;;
    *) echo 'usage: finish-rad4.sh /root|/' >&2; exit 2 ;;
esac

stock="$root/usr/cpu/sysgen/IP27boot"
boot="$root/var/sysgen/boot"
master="$root/var/sysgen/master.d/rad4pci"
xservers="$root/var/X11/xdm/Xservers"
kernel="$root/unix"

for path in "$stock" "$boot"; do
    if [ ! -d "$path" ]; then
        echo "missing installed kernel directory: $path" >&2
        exit 1
    fi
done
for path in "$master" "$xservers" "$boot/rad4pci.o" "$root/usr/sbin/lboot"; do
    if [ ! -f "$path" ]; then
        echo "missing installed file: $path" >&2
        exit 1
    fi
done

needs_kernel=no

# The RAD4 package adds its object to /var/sysgen/boot, while stock IP27
# objects can remain under /usr/cpu/sysgen/IP27boot after the miniroot install.
for source in "$stock"/*; do
    [ -f "$source" ] || continue
    name=`basename "$source"`
    target="$boot/$name"
    if [ ! -r "$target" ]; then
        ln -s "../../../usr/cpu/sysgen/IP27boot/$name" "$target"
        needs_kernel=yes
    fi
done

if grep 'rad4pci_no_dma = 0' "$master" >/dev/null; then
    sed 's/rad4pci_no_dma = 0/rad4pci_no_dma = 1/' "$master" > "$master.sgi-new"
    mv "$master.sgi-new" "$master"
    needs_kernel=yes
elif ! grep 'rad4pci_no_dma = 1' "$master" >/dev/null; then
    echo "RAD4 DMA setting not found: $master" >&2
    exit 1
fi

wanted=':0 secure /var/rad4/X -kybd /dev/input/keyboard -pntr /dev/input/mouse'
if [ -s "$xservers" ]; then
    if [ "`cat "$xservers"`" != "$wanted" ]; then
        echo "existing X server configuration needs review: $xservers" >&2
        exit 1
    fi
else
    echo "$wanted" > "$xservers"
fi

if [ -f "$kernel.install" ]; then
    if [ -f "$kernel.install.sgi-prior" ]; then
        echo "previous kernel build needs review: $kernel.install.sgi-prior" >&2
        exit 1
    fi
    mv "$kernel.install" "$kernel.install.sgi-prior"
fi

if [ -n "$root" ]; then
    chroot "$root" /sbin/chkconfig windowsystem on
    chroot "$root" /etc/autoconfig -v
else
    /sbin/chkconfig windowsystem on
    /etc/autoconfig -v
fi

if [ -s "$kernel.install" ]; then
    if [ -f "$kernel" ] && [ ! -f "$kernel.sgi-before-rad4" ]; then
        cp "$kernel" "$kernel.sgi-before-rad4"
    fi
    mv "$kernel.install" "$kernel"
elif [ "$needs_kernel" = yes ]; then
    echo 'autoconfig did not produce a new kernel after RAD4 changes' >&2
    exit 1
fi

echo 'RAD4 configuration written; reboot the installed guest.'
