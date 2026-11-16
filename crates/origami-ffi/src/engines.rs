//! The engines a build links: FakeEngine alone under `fake`, else QEMU.

use origami_core::facade::CoreError;
use origami_engine::Engine;
use std::sync::Arc;

pub(crate) struct Engines {
    pub list: Vec<Arc<dyn Engine>>,
    #[cfg(feature = "fake")]
    pub fake: Arc<origami_engine::fake::FakeEngine>,
}

#[cfg(feature = "fake")]
pub(crate) fn build() -> Result<Engines, CoreError> {
    let fake = Arc::new(origami_engine::fake::FakeEngine::new());
    Ok(Engines {
        list: vec![fake.clone()],
        fake,
    })
}

#[cfg(all(feature = "qemu", not(feature = "fake")))]
pub(crate) fn build() -> Result<Engines, CoreError> {
    let runtime = origami_qemu::Runtime::locate().map_err(CoreError::machine)?;
    Ok(Engines {
        list: vec![Arc::new(origami_qemu::QemuEngine::new(runtime))],
    })
}

#[cfg(not(any(feature = "fake", feature = "qemu")))]
pub(crate) fn build() -> Result<Engines, CoreError> {
    Err(CoreError::new(
        origami_core::facade::ErrorKind::Unsupported,
        "this build of origami-ffi links no engine",
    ))
}
