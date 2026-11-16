//! The GPU side: one device for every window, a texture for each display
//! head that rows are uploaded into, and our own pass that scales a head into
//! the area of the view that shows it. Toolkit-free: it speaks wgpu, and a
//! window toolkit or a native host supplies the surfaces.

mod blit;
mod compose;
mod device;
mod head;
mod import;
#[cfg(target_os = "macos")]
mod metal;
mod view;
#[cfg(any(target_os = "linux", windows))]
mod vulkan;

pub use blit::{cursor_texture, Blit, CursorDraw, Presenter, ScaleMode};
pub use compose::Compose;
pub use device::Gpu;
pub use head::HeadTexture;
pub use import::{GpuImporter, ImportedBuffer};
pub use raw_window_handle;
pub use view::{Drawn, HostSurface, View};
