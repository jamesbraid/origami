//! The GPU side: one device for every window, a texture for each display
//! head that rows are uploaded into, and our own pass that scales a head into
//! the area of the view that shows it.

mod blit;
mod device;
mod head;

pub use blit::{cursor_texture, Blit, CursorDraw, Presenter, ScaleMode};
pub use device::Gpu;
pub use head::HeadTexture;
