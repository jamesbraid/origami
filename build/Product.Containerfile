FROM docker.io/library/debian:trixie

# vcpkg builds GLib, pixman and SDL; the X11, Wayland, EGL, GL, ALSA,
# PulseAudio and udev headers are for host libraries that stay dynamic.
# The CLI's dependencies need a newer Rust than trixie's 1.85; backports has it.
RUN echo "deb http://deb.debian.org/debian trixie-backports main" > /etc/apt/sources.list.d/backports.list \
 && apt-get update && apt-get install -y --no-install-recommends \
    build-essential git pkg-config python3 python3-venv cmake ninja-build \
    meson flex bison golang-go ca-certificates \
    curl zip unzip autoconf automake libtool autoconf-archive \
    libcap-ng-dev libattr1-dev libfdt-dev \
    libx11-dev libxext-dev libxcursor-dev libxi-dev libxfixes-dev libxrandr-dev libxss-dev \
    libxkbcommon-dev libwayland-dev libdecor-0-dev libegl-dev libgl-dev \
    libudev-dev libasound2-dev libpulse-dev \
 && apt-get install -y --no-install-recommends -t trixie-backports \
    cargo rustc rustfmt rust-clippy \
 && rm -rf /var/lib/apt/lists/*
