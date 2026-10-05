FROM docker.io/library/debian:trixie

# The CLI's dependencies need a newer Rust than trixie's 1.85; backports has it.
RUN echo "deb http://deb.debian.org/debian trixie-backports main" > /etc/apt/sources.list.d/backports.list \
 && apt-get update && apt-get install -y --no-install-recommends \
    build-essential git pkg-config python3 python3-venv cmake ninja-build \
    meson flex bison patchelf golang-go libglib2.0-dev libpixman-1-dev \
    zlib1g-dev libcap-ng-dev libattr1-dev libfdt-dev libsdl2-dev libepoxy-dev \
    libasound2-dev libpulse-dev ca-certificates \
 && apt-get install -y --no-install-recommends -t trixie-backports \
    cargo rustc rustfmt rust-clippy \
 && rm -rf /var/lib/apt/lists/*
