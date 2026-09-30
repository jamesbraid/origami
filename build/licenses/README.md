# Dependency notices

`libslirp-v4.8.0.copyright` is the unchanged [Debian copyright inventory for
libslirp 4.8.0](https://sources.debian.org/data/main/libs/libslirp/4.8.0-1%2Bdeb13u1/debian/copyright).
It supplements upstream `COPYRIGHT` with per-file copyrights and MIT terms.
The inventory comes from Debian package `4.8.0-1+deb13u1`. The emulator builds
upstream 4.8.0 with the patch in its QEMU checkout.

The packagers select this notice by the source directory in QEMU's libslirp
wrap. Review the notice when the upstream version or source hash changes, even
if the directory name stays the same. The generic `foo` wording in the license
paragraphs is retained template text. File attribution comes from the
`Copyright` fields.

## Standard libraries

The archives also include notices for the Rust and Go standard libraries linked
into the CLI and installer. Linux uses Debian's copyright inventory for
`libstd-rust-dev` and the packages owning the Go compiler and runtime source. Windows uses
Fedora's Rust compiler and target-library license files, plus Debian's Go
inventory from its cross-build container. These inventories can describe more
code than the final executable links.

macOS copies Rust's standard-library copyright inventory and its referenced
license texts from the compiler's sysroot. It copies Go's root license and
supplemental notice files from the official toolchain's source tree, retaining
their paths. Compiler versions accompany these notices under
`share/sgi/licenses/toolchains`. Crate and module notices remain separate.

The macOS notice paths still need verification in a native package build.
