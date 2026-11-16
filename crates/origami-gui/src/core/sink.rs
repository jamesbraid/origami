use super::engine::Result;
use super::InputEvent;

/// Where an engine sends the guest's input. One per machine, used from one
/// thread. The QEMU engine's writes binary messages on its display socket;
/// another channel replaces it by implementing this and being handed to the
/// engine.
pub trait InputSink: Send {
    /// Deliver `events` in order, as one batch.
    fn send(&mut self, events: &[InputEvent]) -> Result<()>;
}
