use super::status::Facts;
use super::{
    ConsoleKind, ConsoleLink, CursorInfo, Frame, HeadInfo, InputEvent, MachineId, Power, Result,
    Wake,
};
use std::path::{Path, PathBuf};

/// Something that can run machines. Implemented for QEMU; iris is meant to
/// follow, which is why nothing here assumes a process or a socket.
pub trait Engine: Send + Sync {
    /// A short stable name for the engine, as a machine would select it.
    fn id(&self) -> &'static str;

    /// Where `wake` is called from when a machine has new frames.
    fn set_wake(&self, wake: Wake);

    /// Start the machine kept in `dir`, or attach to it when it is already
    /// running. The machine runs until [`Engine::stop`], whatever views come
    /// and go.
    fn start(&self, dir: &Path) -> Result<MachineId>;

    /// False once the machine has gone away on its own (the guest powered
    /// off, QEMU exited). Engines call the wake callback when it changes.
    fn is_running(&self, machine: MachineId) -> bool;

    /// Stop the machine cleanly and forget it.
    fn stop(&self, machine: MachineId) -> Result<()>;

    /// The machine's heads in order, with their sizes and sequence numbers.
    fn heads(&self, machine: MachineId) -> Result<Vec<HeadInfo>>;

    /// The rows of `head` that changed since the last call, or `None` when
    /// nothing did. When the engine has the head's memory on the GPU the rows
    /// are named and not copied ([`Frame::Imported`]); otherwise they are
    /// copied into `pixels` (cleared first). One consumer per head: a second
    /// caller would see only what the first left.
    fn take_frame(
        &self,
        machine: MachineId,
        head: usize,
        pixels: &mut Vec<u8>,
    ) -> Result<Option<Frame>>;

    /// How the machine's pictures reach the GPU, in a few words for a debug
    /// display ("vulkan import", "shared memory copy"), or None before it
    /// has any.
    fn display_backend(&self, _machine: MachineId) -> Option<String> {
        None
    }

    /// Tell the engine whether anyone is looking, so it can skip work for a
    /// machine nobody sees. Changes keep accumulating while invisible; the
    /// next [`Engine::take_frame`] returns them. Calling it again with the
    /// same value does nothing.
    fn set_visible(&self, machine: MachineId, visible: bool);

    /// The size in pixels of the area a head is shown in. Only a hint:
    /// engines that cannot render at another size ignore it.
    fn set_size(&self, _machine: MachineId, _head: usize, _width: u32, _height: u32) {}

    /// The guest cursor of `head`, or None where the engine has none. The
    /// image is returned only when it differs from `known_gen`.
    fn cursor(&self, _machine: MachineId, _head: usize, _known_gen: u32) -> Option<CursorInfo> {
        None
    }

    /// Deliver one typed input event to the guest. Never blocks on the guest.
    fn send_input(&self, machine: MachineId, event: InputEvent) -> Result<()>;

    /// The machine's directory, where its logs are.
    fn dir(&self, machine: MachineId) -> Option<PathBuf>;

    /// Where the machine's display connection stands, for naming why there
    /// is no picture. The runner log's error is not here: the caller reads
    /// that file and adds it.
    fn facts(&self, machine: MachineId) -> Facts;

    /// Lines for a debug display: the protocol's state and counters.
    fn debug(&self, _machine: MachineId) -> Vec<String> {
        Vec::new()
    }

    /// Start, stop or reset the machine. It returns when that is done, which
    /// for a stop can be minutes, so call it from a thread that can wait.
    fn power(&self, machine: MachineId, action: Power) -> Result<()>;

    /// Send bytes to the guest's serial console. Fails while the console is
    /// not connected.
    fn console_send(&self, machine: MachineId, bytes: &[u8]) -> Result<()>;

    /// Move what the console has sent since the last call onto `out`, and
    /// say whether it is connected. Output that arrives while nobody calls
    /// is kept, up to a limit.
    fn console_take(&self, machine: MachineId, out: &mut Vec<u8>) -> ConsoleLink;

    /// The console the PROM uses. Readable only while the machine is stopped.
    fn console_kind(&self, machine: MachineId) -> Result<ConsoleKind>;

    /// Choose the console the PROM uses. Only a stopped machine takes it.
    fn set_console_kind(&self, machine: MachineId, kind: ConsoleKind) -> Result<()>;
}
