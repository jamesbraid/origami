#!/bin/zsh
# Build Origami for macOS: the core's FFI library, its Swift bindings from
# UniFFI, then the Swift app compiled against both and wrapped in Origami.app.
# Needs only the command-line tools (swiftc); there is no Xcode project.
#
#   apps/macos/build.sh
#   APP_OUT=/path/Origami.app          where to put the bundle (default: apps/macos/build)
#   CARGO_TARGET_DIR=...               the Rust build directory (default: <repo>/target)
#   ORIGAMI_PRODUCT=/path/to/product   an extracted product archive; its runner, QEMU
#                                      and instigator are copied in so machines can run
#   ORIGAMI_DEFAULT_LIBRARY=DIR        the library folder the app opens unless told otherwise
#   ORIGAMI_FFI_FEATURES=fake          build the fake engine instead of QEMU (the
#                                      self-test the nightly job runs)
set -eu
here=${0:A:h}
root=${here:h:h}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
features=${ORIGAMI_FFI_FEATURES:-}
out=$here/build
mkdir -p $out
cd $root

cargo_features=()
swift_defines=()
if [[ -n $features ]]; then
    cargo_features=(--features $features)
    [[ $features == *fake* ]] && swift_defines=(-D FAKE)
fi
cargo build --release -p origami-ffi $cargo_features --lib
dylib=$CARGO_TARGET_DIR/release/liborigami_ffi.dylib
gen=$out/gen; rm -rf $gen; mkdir -p $gen
cargo run --release -q -p origami-ffi $cargo_features --bin uniffi-bindgen -- \
    generate --library $dylib --language swift --out-dir $gen

app=${APP_OUT:-$out/Origami.app}
rm -rf $app; mkdir -p $app/Contents/MacOS $app/Contents/Resources $app/Contents/Frameworks
# Link against a copy whose install name is relative to the bundle.
cp $dylib $app/Contents/Frameworks/liborigami_ffi.dylib
install_name_tool -id @rpath/liborigami_ffi.dylib $app/Contents/Frameworks/liborigami_ffi.dylib
include=$root/crates/origami-ffi/include
swiftc -O -swift-version 5 -target arm64-apple-macos14.0 $swift_defines \
    -o $app/Contents/MacOS/OrigamiApp \
    -Xcc -fmodule-map-file=$gen/origami_ffiFFI.modulemap \
    -Xcc -fmodule-map-file=$include/module.modulemap -I $include \
    $gen/origami_ffi.swift ${(f)"$(find $here/Sources -name '*.swift' | sort)"} \
    -L $app/Contents/Frameworks -lorigami_ffi \
    -Xlinker -rpath -Xlinker @executable_path/../Frameworks \
    -framework SwiftUI -framework AppKit -framework QuartzCore -framework Metal -framework Foundation

cp $root/resources/identity/granite-tile.png $app/Contents/Resources/granite-tile.png
# Machine art: the treated stills at 2x and the manifest, without the raw renders.
if [[ -d $root/resources/identity/stills ]]; then
    rsync -a --include='*/' --include='*@2x.png' --include='manifest.json' --exclude='*' \
        $root/resources/identity/stills/ $app/Contents/Resources/stills/
fi

# The QEMU engine looks for its programs relative to the executable.
if [[ -n ${ORIGAMI_PRODUCT:-} ]]; then
    cp -R $ORIGAMI_PRODUCT/libexec $app/Contents/libexec
    [[ -d $ORIGAMI_PRODUCT/share ]] && cp -R $ORIGAMI_PRODUCT/share $app/Contents/share
    # The runner (origami) and instigator are looked up beside the executable;
    # the app is not called Origami because macOS file names ignore case.
    cp $ORIGAMI_PRODUCT/bin/* $app/Contents/MacOS/
fi

library_xml=$(print -rn -- "${ORIGAMI_DEFAULT_LIBRARY:-}" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g')
cat > $app/Contents/Info.plist <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>Origami</string>
<key>CFBundleDisplayName</key><string>Origami</string>
<key>CFBundleIdentifier</key><string>fans.irix.origami</string>
<key>CFBundleExecutable</key><string>OrigamiApp</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>0</string>
<key>CFBundleShortVersionString</key><string>0.6.0-dev</string>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
${library_xml:+<key>OrigamiDefaultLibrary</key><string>$library_xml</string>}
<key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict></plist>
PLIST
# Apple silicon refuses unsigned code; ad hoc is enough to run.
codesign --force --sign - $app/Contents/Frameworks/liborigami_ffi.dylib
codesign --force --sign - $app
echo $app
