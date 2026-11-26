# Instigator configs

Copies of the install configurations Instigator v0.6.0 ships in `configs/`.
The product embeds them (`src/install.rs`) so that the install recipes
match the Instigator build in `runtime.lock`. `cargo xtask runtime` fails if
the pinned Instigator archive carries different files, and
`cargo xtask bump instigator TAG` refreshes them.
