#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
    printf 'usage: %s QEMU-SOURCE SCRATCH-DIR REVISION\n' "$0" >&2
    exit 2
fi

source=$1
scratch=$2
revision=$3
snapshot="$scratch/qemu-source"
marker="$snapshot/.sgi-source-revision"
if [ -e "$snapshot" ]; then
    if [ ! -f "$marker" ] || [ "$(cat "$marker")" != "$revision" ]; then
        printf 'QEMU source snapshot does not match %s: %s\n' "$revision" "$snapshot" >&2
        exit 2
    fi
else
    temporary=$(mktemp -d "$scratch/qemu-source.XXXXXXXX")
    git -C "$source" archive --format=tar --output="$temporary/source.tar" "$revision"
    tar -xf "$temporary/source.tar" -C "$temporary"
    rm "$temporary/source.tar"
    printf '%s\n' "$revision" > "$temporary/.sgi-source-revision"
    mv "$temporary" "$snapshot"
fi
printf '%s\n' "$snapshot"
