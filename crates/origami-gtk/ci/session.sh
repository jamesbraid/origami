#!/bin/sh
# Runs inside the container. usage: session.sh OUTDIR x11|weston|cage [app args]
# Needs: APP, WORK, ORIGAMI_BIN, ORIGAMI_RUNTIME_DIR, ORIGAMI_CACHE_DIR in the environment.
set -eu
OUT=$1; MODE=$2; shift 2
mkdir -p "$OUT" "$WORK"
export XDG_RUNTIME_DIR=$(mktemp -d /tmp/xdg.XXXXXX); chmod 700 "$XDG_RUNTIME_DIR"
unset WAYLAND_DISPLAY DISPLAY || true
export NO_AT_BRIDGE=1
[ -n "${GSK_RENDERER:-}" ] || unset GSK_RENDERER
[ -n "${GDK_DEBUG:-}" ] || unset GDK_DEBUG
if [ -n "${NVIDIA_RUN:-}" ] && [ "$MODE" != x11 ]; then
  export __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/10_nvidia.json
  export __GLX_VENDOR_LIBRARY_NAME=nvidia GBM_BACKEND=nvidia-drm
  export WLR_RENDER_DRM_DEVICE=$(ls /dev/dri/renderD* | head -1)
fi
case $MODE in
x11)
  Xvfb :99 -screen 0 1600x1300x24 >"$OUT/xvfb.log" 2>&1 &
  sleep 2; export DISPLAY=:99 GDK_BACKEND=x11
  openbox >"$OUT/openbox.log" 2>&1 &
  sleep 1
  python3 "$(dirname "$0")/driver.py" "$OUT" x11 "$@"
  ;;
weston)
  weston --backend=headless --renderer=gl --width=1600 --height=1300 --socket=wl-test >"$OUT/weston.log" 2>&1 &
  sleep 3; export WAYLAND_DISPLAY=wl-test GDK_BACKEND=wayland
  python3 "$(dirname "$0")/driver.py" "$OUT" wayland "$@"
  ;;
cage)
  if [ -n "${NVIDIA_RUN:-}" ]; then R=gles2; else R=pixman; fi
  export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=${WLR_RENDERER:-$R} GDK_BACKEND=wayland
  export WLR_HEADLESS_OUTPUTS=1
  cage -s -- python3 "$(dirname "$0")/driver.py" "$OUT" wayland "$@" >"$OUT/cage.log" 2>&1
  ;;
esac
