#!/bin/sh
# In the CI image: runs a few origami commands and renders each transcript to a PNG (OUTDIR).
# Needs ORIGAMI_BIN, WORK, ORIGAMI_RUNTIME_DIR and ORIGAMI_CACHE_DIR; usage: screenshots-cli.sh OUTDIR
set -eu
OUT=$1
mkdir -p "$OUT" "$WORK/cli"
cd "$WORK/cli"
rm -rf demo
# The transcripts show `origami`, not a build path.
mkdir -p bin && ln -sf "$ORIGAMI_BIN" bin/origami
export PATH="$PWD/bin:$PATH" NO_COLOR=1 COLUMNS=100

# render NAME FILE: the transcript as a dark terminal window, prompt lines in green.
render() {
  name=$1; file=$2
  lines=$(wc -l < "$file")
  longest=$(awk '{ if (length($0) > m) m = length($0) } END { print m }' "$file")
  set -- -size "$((longest * 10 + 50))x$((lines * 22 + 40))" xc:'#1e1e1e' -font DejaVu-Sans-Mono -pointsize 16
  y=32
  while IFS= read -r line; do
    colour='#d4d4d4'
    case $line in '$ '*) colour='#7ee787' ;; esac
    escaped=$(printf '%s' "$line" | sed 's/%/%%/g')
    set -- "$@" -fill "$colour" -annotate "+20+$y" "$escaped"
    y=$((y + 22))
  done < "$file"
  convert "$@" -strip -define png:compression-level=9 "$OUT/$name.png"
}

# step FILE CMD...: print "$ CMD" and what it prints (wrapped at 80 columns), keeping going when it fails.
step() {
  f=$1; shift
  printf '$ %s\n' "$*" >> "$f"
  "$@" > step.out 2>&1 || echo "(exit status $?)" >> step.out
  fold -s -w 80 step.out >> "$f"
}

: > machines.txt
step machines.txt origami machines
render cli-machines machines.txt

: > create.txt
step create.txt origami create demo --preset octane2-vpro
step create.txt origami validate demo
render cli-create create.txt

: > run.txt
step run.txt origami run demo --display none --background
sleep 5
step run.txt origami status demo
step run.txt origami stop demo
step run.txt origami status demo
render cli-run run.txt
