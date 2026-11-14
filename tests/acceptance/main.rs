//! Acceptance tests that need a running guest. An acceptance test asserts a
//! stated outcome; this one is that IRIX installs from the public media and
//! boots. It is slow, so a workflow runs it weekly.
//!
//! Run it with a product tree and a scratch directory:
//!
//! ```sh
//! ORIGAMI_PRODUCT=/path/to/tree ORIGAMI_WORK=/scratch/it \
//!     cargo test --test acceptance -- --ignored
//! ```

#[path = "../integration/support/mod.rs"]
mod support;

mod install;
