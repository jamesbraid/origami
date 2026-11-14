//! Component tests: the `origami` binary against the fake runtime in
//! `tests/bin/fake.rs`, which stands in for QEMU, its tools and Instigator.
//! They run on every platform the product does. `tests/runtime` runs some
//! of the same commands against the real QEMU.

#[path = "../support/mod.rs"]
mod support;

#[cfg(unix)]
mod broken_pipe;
mod config_errors;
mod drive_attach_chd;
mod drive_create;
mod emu_id;
mod install_apply;
mod install_logging;
mod lifecycle;
mod networks;
mod pipes;
mod processor;
mod shares;
mod stop;
mod subnet;
mod upgrade;
