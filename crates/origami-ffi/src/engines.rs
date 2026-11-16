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

/// QEMU runs in the `origami` runner beside this library; this process
/// attaches to its display as a viewer.
#[cfg(all(feature = "qemu", not(feature = "fake")))]
pub(crate) fn build() -> Result<Engines, CoreError> {
    let runtime = origami_qemu::Runtime::locate().map_err(CoreError::machine)?;
    let runner = origami_qemu::RunnerEngine::locate_runner().map_err(CoreError::machine)?;
    let engine = origami_qemu::RunnerEngine::new(runtime, runner);
    #[cfg(feature = "render")]
    engine.set_importer(Arc::new(lazy::LazyImporter::default()));
    Ok(Engines {
        list: vec![Arc::new(engine)],
    })
}

/// The one GPU device of the process: views draw with it and the display
/// client imports the guest's picture into it.
#[cfg(feature = "render")]
pub(crate) mod gpu {
    use origami_render::Gpu;
    use std::sync::OnceLock;

    pub(crate) fn shared() -> Result<Gpu, String> {
        static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
        GPU.get_or_init(Gpu::new).clone()
    }
}

/// Imports into [`gpu::shared`], opened at the first machine start. With no
/// GPU that imports, pictures take the copying path.
#[cfg(all(feature = "qemu", feature = "render", not(feature = "fake")))]
mod lazy {
    use super::gpu::shared;
    use origami_display::{Handle, Importer, Memory};
    use origami_engine::GpuMemory;
    use origami_render::GpuImporter;
    use std::sync::{Arc, OnceLock};

    #[derive(Default)]
    pub(crate) struct LazyImporter(OnceLock<Option<Arc<GpuImporter>>>);

    impl LazyImporter {
        fn get(&self) -> Option<&Arc<GpuImporter>> {
            self.0
                .get_or_init(|| shared().ok().and_then(|gpu| GpuImporter::new(&gpu)))
                .as_ref()
        }
    }

    impl Importer for LazyImporter {
        fn caps(&self) -> u32 {
            self.get().map_or(0, |i| i.caps())
        }
        fn device_uuid(&self) -> [u8; 16] {
            self.get().map_or([0; 16], |i| i.device_uuid())
        }
        fn export_uuid(&self) -> Option<[u8; 16]> {
            self.get()?.export_uuid()
        }
        fn accepts(&self, device: &[u8; 16], driver: &[u8; 16]) -> Result<(), String> {
            match self.get() {
                Some(i) => i.accepts(device, driver),
                None => Err("this process has no GPU that imports memory".into()),
            }
        }
        fn import(
            &self,
            region: &origami_display::proto::Region,
            handle: Handle,
        ) -> std::io::Result<Arc<dyn GpuMemory>> {
            match self.get() {
                Some(i) => i.import(region, handle),
                None => Err(std::io::Error::other("no GPU that imports memory")),
            }
        }
        fn wrap(
            &self,
            region: &origami_display::proto::Region,
            memory: &Arc<dyn Memory>,
        ) -> Option<Arc<dyn GpuMemory>> {
            self.get()?.wrap(region, memory)
        }
        fn open_section(
            &self,
            region: &origami_display::proto::Region,
            handle: &Handle,
        ) -> Option<Arc<dyn GpuMemory>> {
            self.get()?.open_section(region, handle)
        }
        fn name(&self) -> &'static str {
            self.get().map_or("none", |i| i.name())
        }
    }
}

#[cfg(not(any(feature = "fake", feature = "qemu")))]
pub(crate) fn build() -> Result<Engines, CoreError> {
    Err(CoreError::new(
        origami_core::facade::ErrorKind::Unsupported,
        "this build of origami-ffi links no engine",
    ))
}
