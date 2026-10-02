FROM docker.io/library/golang:1.26.3-trixie

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential git pkg-config python3 python3-venv cmake ninja-build \
    meson flex bison patchelf cargo rustc rustfmt libglib2.0-dev libpixman-1-dev \
    zlib1g-dev libcap-ng-dev libattr1-dev libfdt-dev libsdl2-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

ENV GOTOOLCHAIN=local
