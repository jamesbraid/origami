//! Integration tests: IRIX guests, run by the real `origami` and its bundled
//! QEMU, on networks with real peers. They answer questions the unit and
//! functional tests cannot, such as whether a guest on a bridge can mount an
//! NFS export from a server on the LAN.
//!
//! The tests are `#[ignore]`d so a plain `cargo test` stays fast. Run them
//! with a product tree and a scratch directory (README.md):
//!
//! ```sh
//! ORIGAMI_PRODUCT=/path/to/tree ORIGAMI_WORK=/scratch/it \
//!     cargo test --test integration -- --ignored --test-threads=1
//! ```

mod lab;
mod scenarios;
mod support;
