FROM docker.io/library/debian:trixie

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential git pkg-config python3 python3-venv cmake ninja-build \
    meson flex bison patchelf cargo rustc rustfmt golang-go libglib2.0-dev libpixman-1-dev \
    zlib1g-dev libcap-ng-dev libattr1-dev libfdt-dev libsdl2-dev libepoxy-dev \
    libasound2-dev libpulse-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*
