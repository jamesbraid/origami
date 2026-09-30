FROM docker.io/library/debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates build-essential git pkg-config python3 python3-venv \
    ninja-build meson flex bison patchelf libglib2.0-dev libpixman-1-dev \
    zlib1g-dev libcap-ng-dev libattr1-dev libfdt-dev libsdl2-dev \
 && rm -rf /var/lib/apt/lists/*
