use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// What the device has reported. Anything here after a run is a failure the
/// tests look for.
#[derive(Default)]
pub struct GpuErrors {
    pub uncaptured: AtomicU64,
    pub lost: AtomicBool,
    pub first: Mutex<Option<String>>,
}

impl GpuErrors {
    pub fn count(&self) -> u64 {
        self.uncaptured.load(Ordering::Relaxed) + u64::from(self.lost.load(Ordering::Relaxed))
    }

    fn note(&self, what: String) {
        eprintln!("origami-gui: GPU: {what}");
        let mut first = self.first.lock().unwrap_or_else(|e| e.into_inner());
        first.get_or_insert(what);
    }
}

/// The instance, adapter, device and queue every window draws with. Slint is
/// handed these, so a texture made here can be shown in any window.
#[derive(Clone)]
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub errors: Arc<GpuErrors>,
}

impl Gpu {
    pub fn new() -> Result<Gpu, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or_default(),
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or_default(),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
            compatible_surface: None,
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        // The device Slint's own setup would make: everything the adapter
        // offers, and limits big enough for a swapchain-sized image.
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("origami-gui"),
            required_features: adapter.features() - wgpu::Features::all_experimental_mask(),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::default(),
        }))
        .map_err(|e| format!("cannot open the GPU: {e}"))?;
        let errors = Arc::new(GpuErrors::default());
        {
            let errors = errors.clone();
            device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
                errors.uncaptured.fetch_add(1, Ordering::Relaxed);
                errors.note(format!("uncaptured error: {e}"));
            }));
        }
        {
            let errors = errors.clone();
            device.set_device_lost_callback(move |reason, message| {
                errors.lost.store(true, Ordering::Relaxed);
                errors.note(format!("device lost ({reason:?}): {message}"));
            });
        }
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            errors,
        })
    }

    pub fn describe(&self) -> String {
        let i = self.adapter.get_info();
        format!("{} ({:?}, {:?})", i.name, i.backend, i.device_type)
    }
}
