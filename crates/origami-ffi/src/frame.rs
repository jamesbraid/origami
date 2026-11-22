//! The C frame plane: pixels, the wake-up, input and presenting into a
//! window. Declared in `include/origami_frame.h`, which cbindgen generates
//! from this file.
//!
//! Ownership and threading, which the header repeats:
//! - `origami_machine*` and `origami_core*` are reference-counted handles you
//!   release once. A machine handle does not keep the guest alive.
//! - Every function may be called from any thread, except the `origami_view_*`
//!   functions, which a view's owner calls from one thread at a time.
//! - Every function catches panics and returns an `origami_status`; the text
//!   of the last failure on the calling thread is `origami_last_error`.
//! - Nothing is copied unless you ask: `origami_frame_take` is the copy path;
//!   `origami_view_present` draws without handing pixels to the caller.

#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]

use crate::keys;
use crate::wake::WakeHandle;
use ::origami_core::facade::{
    self as f, Core, CoreError, ErrorKind, Frame, HookId, InputEvent, Machine, PointerButton,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

/// The version of this ABI, as `origami_frame_abi_version` returns it.
pub const ORIGAMI_FRAME_ABI_VERSION: u32 = 1;
pub(crate) const ABI_VERSION: u32 = ORIGAMI_FRAME_ABI_VERSION;

/// What a frame-plane call returns: `ORIGAMI_OK` (zero) or a negative
/// `ORIGAMI_ERR_*` code.
pub type origami_status = i32;

pub const ORIGAMI_OK: origami_status = 0;
pub const ORIGAMI_ERR_NULL_ARGUMENT: origami_status = -1;
pub const ORIGAMI_ERR_INVALID_ARGUMENT: origami_status = -2;
pub const ORIGAMI_ERR_NOT_RUNNING: origami_status = -3;
/// A buffer was too small; the call says what size it needed and nothing
/// was lost. Call again for the same head with a bigger buffer.
pub const ORIGAMI_ERR_BUFFER_TOO_SMALL: origami_status = -4;
pub const ORIGAMI_ERR_UNSUPPORTED: origami_status = -5;
pub const ORIGAMI_ERR_NO_GPU: origami_status = -6;
pub const ORIGAMI_ERR_PANIC: origami_status = -7;
pub const ORIGAMI_ERR_ENGINE: origami_status = -8;
pub const ORIGAMI_ERR_NOT_FOUND: origami_status = -9;
pub const ORIGAMI_ERR_BUSY: origami_status = -10;
pub const ORIGAMI_ERR_IO: origami_status = -11;
pub const ORIGAMI_ERR_INTERNAL: origami_status = -12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    NullArgument = -1,
    InvalidArgument = -2,
    NotRunning = -3,
    BufferTooSmall = -4,
    Unsupported = -5,
    #[cfg_attr(not(feature = "render"), allow(dead_code))]
    NoGpu = -6,
    Panic = -7,
    Engine = -8,
    NotFound = -9,
    Busy = -10,
    Io = -11,
    Internal = -12,
}

impl Status {
    fn code(self) -> origami_status {
        self as origami_status
    }
}

pub(crate) struct Fail(pub(crate) Status, pub(crate) String);

impl From<CoreError> for Fail {
    fn from(e: CoreError) -> Fail {
        let status = match e.kind {
            ErrorKind::NotFound => Status::NotFound,
            ErrorKind::Invalid => Status::InvalidArgument,
            ErrorKind::Busy => Status::Busy,
            ErrorKind::NotRunning => Status::NotRunning,
            ErrorKind::Unsupported => Status::Unsupported,
            ErrorKind::Io => Status::Io,
            ErrorKind::Engine | ErrorKind::Protocol => Status::Engine,
            ErrorKind::NeedsUpgrade | ErrorKind::Cancelled | ErrorKind::Internal => {
                Status::Internal
            }
        };
        Fail(status, e.message)
    }
}

pub(crate) fn fail<T>(status: Status, message: &str) -> Result<T, Fail> {
    Err(Fail(status, message.into()))
}

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

pub(crate) fn guard(body: impl FnOnce() -> Result<(), Fail>) -> origami_status {
    let (status, message) = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => return ORIGAMI_OK,
        Ok(Err(Fail(status, message))) => (status, message),
        Err(_) => (Status::Panic, "a panic was caught at the boundary".into()),
    };
    LAST_ERROR.with(|last| *last.borrow_mut() = message);
    status.code()
}

/// Copy the text of the calling thread's last failure into `buf` (NUL
/// terminated, truncated to fit) and return its full length without the NUL.
#[no_mangle]
pub unsafe extern "C" fn origami_last_error(buf: *mut c_char, cap: usize) -> usize {
    LAST_ERROR.with(|last| {
        let text = last.borrow();
        // SAFETY: the caller promises `buf` has `cap` writable bytes.
        unsafe { copy_text(&text, buf, cap) };
        text.len()
    })
}

unsafe fn copy_text(text: &str, buf: *mut c_char, cap: usize) {
    if buf.is_null() || cap == 0 {
        return;
    }
    let n = text.len().min(cap - 1);
    // SAFETY: `n + 1 <= cap` bytes are writable and `text` has `n` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buf.cast::<u8>(), n);
        *buf.add(n) = 0;
    }
}

/// The version of this ABI; compare with `ORIGAMI_FRAME_ABI_VERSION`.
#[no_mangle]
pub extern "C" fn origami_frame_abi_version() -> u32 {
    ABI_VERSION
}

// --- machines ---------------------------------------------------------------

#[derive(Default)]
struct Scratch {
    pixels: Vec<u8>,
    /// A frame taken from the engine that did not fit the caller's buffers.
    pending: Option<(u32, f::FrameUpdate)>,
}

struct Callback {
    run: unsafe extern "C" fn(*mut c_void),
    user: *mut c_void,
}

// SAFETY: the caller of `origami_frame_set_wake` promises the callback and
// its user data may be used from any thread.
unsafe impl Send for Callback {}
// SAFETY: as above.
unsafe impl Sync for Callback {}

#[derive(Default)]
struct WakeState {
    callback_hook: Option<HookId>,
    handle: Option<Arc<WakeHandle>>,
    handle_hook: Option<HookId>,
}

/// A machine as the frame plane sees it. Opaque to C.
pub struct origami_machine {
    machine: Machine,
    scratch: Mutex<Scratch>,
    wake: Mutex<WakeState>,
    callback: Arc<RwLock<Option<Callback>>>,
    /// The small picture a cube reflects, drawn by the view that presents the head.
    #[cfg(feature = "render")]
    pub(crate) reflection: Mutex<Option<(u32, Weak<origami_render::Reflection>)>>,
}

#[cfg(feature = "render")]
impl origami_machine {
    /// The cube picture of `head`, when a cube wants one.
    pub(crate) fn reflection(&self, head: u32) -> Option<Arc<origami_render::Reflection>> {
        let mut slot = self.reflection.lock().unwrap_or_else(|e| e.into_inner());
        let (wanted, weak) = slot.as_ref()?;
        if *wanted != head {
            return None;
        }
        let found = weak.upgrade();
        if found.is_none() {
            *slot = None;
        }
        found
    }
}

impl Drop for origami_machine {
    fn drop(&mut self) {
        let wake = self.wake.get_mut().unwrap_or_else(|e| e.into_inner());
        for hook in [wake.callback_hook.take(), wake.handle_hook.take()]
            .into_iter()
            .flatten()
        {
            self.machine.remove_frame_hook(hook);
        }
        // The hooks are gone; clearing the slot also waits for a call that
        // is already running, so the callback never fires after the last
        // release returns.
        *self.callback.write().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn registry() -> &'static Mutex<HashMap<String, Weak<origami_machine>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Weak<origami_machine>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// The one frame-plane object of `machine`, so a wake handle and a pending
/// frame are shared by everyone who asks for the same machine.
fn frame_machine(machine: &Machine) -> Arc<origami_machine> {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    registry.retain(|_, weak| weak.strong_count() > 0);
    if let Some(found) = registry.get(machine.key()).and_then(Weak::upgrade) {
        return found;
    }
    let made = Arc::new(origami_machine {
        machine: machine.clone(),
        scratch: Mutex::default(),
        wake: Mutex::default(),
        callback: Arc::default(),
        #[cfg(feature = "render")]
        reflection: Mutex::default(),
    });
    registry.insert(machine.key().to_string(), Arc::downgrade(&made));
    made
}

/// An owned handle on `machine` for the C frame plane; release it with
/// `origami_machine_release`.
pub fn retain_machine(machine: &Machine) -> *const origami_machine {
    Arc::into_raw(frame_machine(machine))
}

pub(crate) unsafe fn machine_ref<'a>(
    p: *const origami_machine,
) -> Result<&'a origami_machine, Fail> {
    // SAFETY: a non-null pointer came from `Arc::into_raw` and is live until released.
    unsafe { p.as_ref() }.ok_or_else(|| Fail(Status::NullArgument, "machine is null".into()))
}

unsafe fn text<'a>(p: *const c_char, what: &str) -> Result<&'a str, Fail> {
    if p.is_null() {
        return fail(Status::NullArgument, &format!("{what} is null"));
    }
    // SAFETY: the caller promises a NUL terminated string.
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|_| Fail(Status::InvalidArgument, format!("{what} is not UTF-8")))
}

/// Release a machine handle from `origami_machine_open` or
/// `OrigamiMachine.frame_plane_handle`. Null is ignored. Releasing the last
/// handle of a machine clears its wake callback and waits for a running call
/// to finish, so do not release from inside the callback.
#[no_mangle]
pub unsafe extern "C" fn origami_machine_release(machine: *const origami_machine) {
    if !machine.is_null() {
        // SAFETY: the handle holds one strong count, given up here.
        let machine = unsafe { Arc::from_raw(machine) };
        // A panic in a drop must not unwind into C.
        let _ = catch_unwind(AssertUnwindSafe(move || drop(machine)));
    }
}

// --- a small core for plain C -----------------------------------------------

/// A core made by `origami_core_new`, for programs without the control plane.
/// C sees it as `origami_core` (see cbindgen.toml); the Rust name avoids the
/// crate of that name.
pub struct CoreHandle {
    core: Core,
    #[cfg(feature = "fake")]
    fake: Arc<origami_engine::fake::FakeEngine>,
}

/// Make a core whose machine library is the folder `library_root`. In a
/// `fake` build the only engine is the fake one.
#[no_mangle]
pub unsafe extern "C" fn origami_core_new(
    library_root: *const c_char,
    out: *mut *mut CoreHandle,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let root = unsafe { text(library_root, "library_root")? };
        if out.is_null() {
            return fail(Status::NullArgument, "out is null");
        }
        let engines = crate::engines::build()?;
        let core = Core::new(f::Config::new(root), engines.list, None)?;
        let made = Box::new(CoreHandle {
            core,
            #[cfg(feature = "fake")]
            fake: engines.fake,
        });
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = Box::into_raw(made) };
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn origami_core_release(core: *mut CoreHandle) {
    if !core.is_null() {
        // SAFETY: made by `origami_core_new`, released once.
        let core = unsafe { Box::from_raw(core) };
        let _ = catch_unwind(AssertUnwindSafe(move || core.core.shutdown()));
    }
}

/// Write the library as lines `KEY<TAB>NAME<LF>` into `buf` (NUL terminated,
/// truncated to fit). `needed` receives the full length without the NUL.
#[no_mangle]
pub unsafe extern "C" fn origami_core_machines(
    core: *const CoreHandle,
    buf: *mut c_char,
    cap: usize,
    needed: *mut usize,
) -> origami_status {
    guard(|| {
        // SAFETY: a non-null pointer came from `origami_core_new`.
        let core = unsafe { core.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "core is null".into()))?;
        let list: String = core
            .core
            .list_machines()?
            .iter()
            .map(|m| format!("{}\t{}\n", m.key, m.name))
            .collect();
        // SAFETY: argument contracts are in the header.
        unsafe { copy_text(&list, buf, cap) };
        if !needed.is_null() {
            // SAFETY: `needed` is writable when non-null.
            unsafe { *needed = list.len() };
        }
        Ok(())
    })
}

/// Open the machine `key` (a key from `origami_core_machines`). Release it
/// with `origami_machine_release`.
#[no_mangle]
pub unsafe extern "C" fn origami_machine_open(
    core: *const CoreHandle,
    key: *const c_char,
    out: *mut *const origami_machine,
) -> origami_status {
    guard(|| {
        // SAFETY: as above.
        let core = unsafe { core.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "core is null".into()))?;
        // SAFETY: as above.
        let key = unsafe { text(key, "key")? };
        if out.is_null() {
            return fail(Status::NullArgument, "out is null");
        }
        let machine = core.core.open_machine(key)?;
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = retain_machine(&machine) };
        Ok(())
    })
}

/// Start the guest and wait until it runs. Blocks: not for a UI thread.
#[no_mangle]
pub unsafe extern "C" fn origami_machine_start(machine: *const origami_machine) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        match m.machine.start()?.wait(None) {
            f::OperationState::Failed(error) => Err(error.into()),
            _ => Ok(()),
        }
    })
}

/// Only in builds with the fake engine: give the started machine `key` a new
/// picture, as a running guest would.
#[cfg(feature = "fake")]
#[no_mangle]
pub unsafe extern "C" fn origami_fake_advance_frame(
    core: *const CoreHandle,
    key: *const c_char,
) -> origami_status {
    guard(|| {
        // SAFETY: as above.
        let core = unsafe { core.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "core is null".into()))?;
        // SAFETY: as above.
        let key = unsafe { text(key, "key")? };
        let machine = core.core.open_machine(key)?;
        let session = core
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| Fail(Status::NotRunning, "the machine was never started".into()))?;
        session.advance_frame();
        Ok(())
    })
}

/// Only in builds with the fake engine: the started machine `key` shows an
/// 8 by 8 white cursor with its hot spot at (`x`, `y`).
#[cfg(feature = "fake")]
#[no_mangle]
pub unsafe extern "C" fn origami_fake_show_cursor(
    core: *const CoreHandle,
    key: *const c_char,
    x: i32,
    y: i32,
) -> origami_status {
    guard(|| {
        // SAFETY: as above.
        let core = unsafe { core.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "core is null".into()))?;
        // SAFETY: as above.
        let key = unsafe { text(key, "key")? };
        let machine = core.core.open_machine(key)?;
        let session = core
            .fake
            .session_in(std::path::Path::new(machine.key()))
            .ok_or_else(|| Fail(Status::NotRunning, "the machine was never started".into()))?;
        session.show_cursor(x, y);
        Ok(())
    })
}

/// Only in builds with the fake engine: make a machine named `name` in the
/// library, as `fake.1`, and write its key into `key_out` (NUL terminated,
/// truncated to fit; may be NULL).
#[cfg(feature = "fake")]
#[no_mangle]
pub unsafe extern "C" fn origami_fake_create_machine(
    core: *const CoreHandle,
    name: *const c_char,
    key_out: *mut c_char,
    cap: usize,
) -> origami_status {
    guard(|| {
        // SAFETY: as above.
        let core = unsafe { core.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "core is null".into()))?;
        // SAFETY: as above.
        let name = unsafe { text(name, "name")? };
        let prom = core.core.root().join(".fake-prom.bin");
        std::fs::create_dir_all(core.core.root()).map_err(CoreError::from)?;
        std::fs::write(&prom, [0u8; 16]).map_err(CoreError::from)?;
        let op = core.core.create_machine(f::CreateRequest {
            name: name.into(),
            offering: "fake.1".into(),
            boot_prom: Some(prom),
            ..Default::default()
        })?;
        if let f::OperationState::Failed(error) = op.wait(None) {
            return Err(error.into());
        }
        // SAFETY: argument contracts are in the header.
        unsafe { copy_text(&op.result_machine().unwrap_or_default(), key_out, cap) };
        Ok(())
    })
}

// --- frames -----------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct origami_head_info {
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    /// 0 while the head has no picture.
    pub active: u32,
    pub reserved: u32,
}

/// Fill `out[0..min(count, cap)]` with the machine's heads; `count` gets the
/// number of heads.
#[no_mangle]
pub unsafe extern "C" fn origami_frame_heads(
    machine: *const origami_machine,
    out: *mut origami_head_info,
    cap: usize,
    count: *mut usize,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        let heads = m.machine.heads();
        if !count.is_null() {
            // SAFETY: `count` is writable when non-null.
            unsafe { *count = heads.len() };
        }
        if out.is_null() && cap > 0 {
            return fail(Status::NullArgument, "out is null");
        }
        for (i, head) in heads.iter().take(cap).enumerate() {
            // SAFETY: `i < cap` and `out` has `cap` writable elements.
            unsafe {
                *out.add(i) = origami_head_info {
                    width: head.width,
                    height: head.height,
                    seq: head.seq,
                    active: u32::from(head.active),
                    reserved: 0,
                };
            }
        }
        Ok(())
    })
}

/// One run of changed lines, `y0` up to but not including `y1`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct origami_run {
    pub y0: u32,
    pub y1: u32,
}

/// No new picture.
pub const ORIGAMI_FRAME_NONE: u32 = 0;
/// The changed rows were copied into the caller's buffer.
pub const ORIGAMI_FRAME_ROWS: u32 = 1;

/// What `origami_frame_take` found. The caller sets `size` to
/// `sizeof(origami_frame)`.
#[repr(C)]
pub struct origami_frame {
    pub size: u32,
    /// `ORIGAMI_FRAME_NONE` or `ORIGAMI_FRAME_ROWS`.
    pub kind: u32,
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    /// The head changed size: the whole picture is new.
    pub resized: u32,
    /// Runs written to the caller's array.
    pub run_count: u32,
    /// Bytes written to the pixel buffer: the changed rows one after another,
    /// 4 bytes a pixel (blue, green, red, unused), tight. With
    /// `BufferTooSmall`, the bytes needed.
    pub bytes_used: u64,
}

/// Copy the rows of `head` that changed since the last call into `pixels`
/// and their line runs into `runs`: the copy path. `out->kind` is
/// `ORIGAMI_FRAME_NONE` when nothing changed. One consumer per head.
///
/// `pixels_cap` of `width * height * 4` and `runs_cap` of `height` always
/// suffice. If either buffer is too small the call returns `BufferTooSmall`
/// with the needed byte and run counts in `out`, and keeps the frame for the
/// next call for this head with bigger buffers.
#[no_mangle]
pub unsafe extern "C" fn origami_frame_take(
    machine: *const origami_machine,
    head: u32,
    pixels: *mut u8,
    pixels_cap: usize,
    runs: *mut origami_run,
    runs_cap: usize,
    out: *mut origami_frame,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        // SAFETY: `out` is writable when non-null.
        let out = unsafe { out.as_mut() }
            .ok_or_else(|| Fail(Status::NullArgument, "out is null".into()))?;
        if (out.size as usize) < std::mem::size_of::<origami_frame>() {
            return fail(Status::InvalidArgument, "out->size is too small");
        }
        let mut scratch = m.scratch.lock().unwrap_or_else(|e| e.into_inner());
        let scratch = &mut *scratch;
        match &scratch.pending {
            Some((pending_head, _)) if *pending_head != head => {
                return fail(
                    Status::Busy,
                    "finish the frame of the head that came back BufferTooSmall first",
                );
            }
            Some(_) => {}
            None => match m.machine.take_frame(head as usize, &mut scratch.pixels)? {
                None => {
                    out.kind = ORIGAMI_FRAME_NONE;
                    out.run_count = 0;
                    out.bytes_used = 0;
                    return Ok(());
                }
                Some(Frame::Rows(update)) => scratch.pending = Some((head, update)),
                Some(Frame::Imported(_)) => {
                    return fail(
                        Status::Unsupported,
                        "the frame is in GPU memory: present it with origami_view_present",
                    );
                }
            },
        }
        let update = &scratch.pending.as_ref().expect("a pending frame").1;
        out.bytes_used = scratch.pixels.len() as u64;
        out.run_count = update.runs.len() as u32;
        if scratch.pixels.len() > pixels_cap || update.runs.len() > runs_cap {
            out.kind = ORIGAMI_FRAME_NONE;
            return fail(
                Status::BufferTooSmall,
                "a buffer is too small for the frame",
            );
        }
        if (scratch.pixels.is_empty() || !pixels.is_null())
            && (update.runs.is_empty() || !runs.is_null())
        {
            // SAFETY: both caps were checked above and the pointers are writable for them.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    scratch.pixels.as_ptr(),
                    pixels,
                    scratch.pixels.len(),
                );
                for (i, &(y0, y1)) in update.runs.iter().enumerate() {
                    *runs.add(i) = origami_run { y0, y1 };
                }
            }
        } else {
            return fail(Status::NullArgument, "pixels or runs is null");
        }
        out.kind = ORIGAMI_FRAME_ROWS;
        out.seq = update.seq;
        out.width = update.width;
        out.height = update.height;
        out.resized = u32::from(update.resized);
        scratch.pending = None;
        Ok(())
    })
}

/// What the display client has moved since the machine started.
#[repr(C)]
pub struct origami_frame_counters {
    pub size: u32,
    /// 0 when the engine does not count; the rest is then zero.
    pub counted: u32,
    pub frames: u64,
    /// Lines handed out, copied or imported.
    pub lines: u64,
    /// Lines the GPU reads in place, a part of `lines`.
    pub imported_lines: u64,
    /// Pixel bytes copied by the CPU: 0 when every picture is imported.
    pub copied_bytes: u64,
}

/// Read the display counters, for tests that zero copy holds.
#[no_mangle]
pub unsafe extern "C" fn origami_frame_stats(
    machine: *const origami_machine,
    out: *mut origami_frame_counters,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        // SAFETY: `out` is writable when non-null.
        let out = unsafe { out.as_mut() }
            .ok_or_else(|| Fail(Status::NullArgument, "out is null".into()))?;
        if (out.size as usize) < std::mem::size_of::<origami_frame_counters>() {
            return fail(Status::InvalidArgument, "out->size is too small");
        }
        let size = out.size;
        let s = m.machine.frame_stats();
        let n = s.unwrap_or_default();
        *out = origami_frame_counters {
            size,
            counted: u32::from(s.is_some()),
            frames: n.frames,
            lines: n.lines,
            imported_lines: n.imported_lines,
            copied_bytes: n.copied_bytes,
        };
        Ok(())
    })
}

/// The guest's cursor on a head.
#[repr(C)]
pub struct origami_cursor {
    pub size: u32,
    /// 0 when the engine draws the cursor into the picture; the rest is then
    /// zero.
    pub present: u32,
    pub on: u32,
    pub x: i32,
    pub y: i32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub width: u32,
    pub height: u32,
    /// Moves when the image changes.
    pub gen: u32,
    /// Bytes of image written, 0 when `gen` is the one the caller passed.
    /// With `BufferTooSmall`, the bytes needed.
    pub image_bytes: u32,
}

/// Read the cursor of `head`. The image (`width * height` 0xAARRGGBB
/// little-endian words) is copied into `image` only when `gen` differs from
/// `known_gen`.
#[no_mangle]
pub unsafe extern "C" fn origami_frame_cursor(
    machine: *const origami_machine,
    head: u32,
    known_gen: u32,
    out: *mut origami_cursor,
    image: *mut u8,
    image_cap: usize,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        // SAFETY: `out` is writable when non-null.
        let out = unsafe { out.as_mut() }
            .ok_or_else(|| Fail(Status::NullArgument, "out is null".into()))?;
        if (out.size as usize) < std::mem::size_of::<origami_cursor>() {
            return fail(Status::InvalidArgument, "out->size is too small");
        }
        let size = out.size;
        let Some(c) = m.machine.cursor(head as usize, known_gen)? else {
            *out = origami_cursor {
                size,
                present: 0,
                on: 0,
                x: 0,
                y: 0,
                hot_x: 0,
                hot_y: 0,
                width: 0,
                height: 0,
                gen: known_gen,
                image_bytes: 0,
            };
            return Ok(());
        };
        let bytes = c.image.as_ref().map_or(0, Vec::len);
        *out = origami_cursor {
            size,
            present: 1,
            on: u32::from(c.on),
            x: c.x,
            y: c.y,
            hot_x: c.hot_x,
            hot_y: c.hot_y,
            width: c.width,
            height: c.height,
            gen: c.gen,
            image_bytes: bytes as u32,
        };
        if let Some(pixels) = &c.image {
            if bytes > image_cap || image.is_null() {
                return fail(Status::BufferTooSmall, "the cursor image does not fit");
            }
            // SAFETY: `image` has `image_cap >= bytes` writable bytes.
            unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), image, bytes) };
        }
        Ok(())
    })
}

// --- the wake-up ------------------------------------------------------------

/// Called on an engine thread when a head has a new picture. Do almost
/// nothing and never block: set a flag or post to your UI thread.
pub type origami_wake_fn = Option<unsafe extern "C" fn(user_data: *mut c_void)>;

/// Call `callback(user_data)` whenever a new frame is published, replacing
/// any earlier callback; NULL removes it. When this returns, no call of the
/// previous callback is running or will start, so its `user_data` may be
/// freed. Do not call this from inside the callback. Allocation free per frame.
/// On a running machine it is also called once before this returns, since a
/// frame may already be waiting.
#[no_mangle]
pub unsafe extern "C" fn origami_frame_set_wake(
    machine: *const origami_machine,
    callback: origami_wake_fn,
    user_data: *mut c_void,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        let mut wake = m.wake.lock().unwrap_or_else(|e| e.into_inner());
        *m.callback.write().unwrap_or_else(|e| e.into_inner()) = callback.map(|run| Callback {
            run,
            user: user_data,
        });
        if wake.callback_hook.is_none() {
            let slot = m.callback.clone();
            wake.callback_hook = Some(m.machine.add_frame_hook(Arc::new(move || {
                let slot = slot.read().unwrap_or_else(|e| e.into_inner());
                if let Some(cb) = slot.as_ref() {
                    // SAFETY: set by the caller of `origami_frame_set_wake`.
                    unsafe { (cb.run)(cb.user) };
                }
            })));
        }
        drop(wake);
        // A frame that came before the callback woke nobody. Called outside
        // the lock, so the callback may call origami_frame_wake_clear.
        if !m.machine.heads().is_empty() {
            let slot = m.callback.read().unwrap_or_else(|e| e.into_inner());
            if let Some(cb) = slot.as_ref() {
                // SAFETY: just set by our caller.
                unsafe { (cb.run)(cb.user) };
            }
        }
        Ok(())
    })
}

/// The handle that is signalled while a new frame is waiting: a readable
/// file descriptor on Unix (an eventfd, a pipe on macOS), an auto-reset event
/// `HANDLE` on Windows, returned in `handle`. It belongs to the machine
/// handle: do not close it, and do not use it after
/// `origami_machine_release`. The same handle comes back on every call.
/// Made for a running machine, it starts signalled.
///
/// Wait on it (`poll`, `kqueue`, `WaitForSingleObject`), then call
/// `origami_frame_take` until it reports no frame; on Unix call
/// `origami_frame_wake_clear` before that to re-arm it. Windows resets on a
/// successful wait. Allocation free per frame.
#[no_mangle]
pub unsafe extern "C" fn origami_frame_wake_handle(
    machine: *const origami_machine,
    handle: *mut isize,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        if handle.is_null() {
            return fail(Status::NullArgument, "handle is null");
        }
        let mut wake = m.wake.lock().unwrap_or_else(|e| e.into_inner());
        let mut made_now = None;
        if wake.handle.is_none() {
            let made = Arc::new(WakeHandle::new().map_err(|e| Fail(Status::Io, e))?);
            let signal = made.clone();
            wake.handle_hook = Some(m.machine.add_frame_hook(Arc::new(move || signal.signal())));
            wake.handle = Some(made.clone());
            made_now = Some(made);
        }
        // SAFETY: `handle` is writable.
        unsafe { *handle = wake.handle.as_ref().expect("made above").raw() };
        drop(wake);
        // A frame that came before the handle woke nobody.
        if let Some(made) = made_now.filter(|_| !m.machine.heads().is_empty()) {
            made.signal();
        }
        Ok(())
    })
}

/// Re-arm the wake handle: make it unreadable until the next frame. A no-op
/// on Windows, whose event resets itself.
#[no_mangle]
pub unsafe extern "C" fn origami_frame_wake_clear(
    machine: *const origami_machine,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        if let Some(handle) = &m.wake.lock().unwrap_or_else(|e| e.into_inner()).handle {
            handle.clear();
        }
        Ok(())
    })
}

// --- input ------------------------------------------------------------------

pub const ORIGAMI_INPUT_KEY: u32 = 0;
pub const ORIGAMI_INPUT_POINTER_MOVE: u32 = 1;
pub const ORIGAMI_INPUT_POINTER_AT: u32 = 2;
pub const ORIGAMI_INPUT_BUTTON: u32 = 3;
pub const ORIGAMI_INPUT_WHEEL: u32 = 4;
pub const ORIGAMI_BUTTON_LEFT: u32 = 0;
pub const ORIGAMI_BUTTON_MIDDLE: u32 = 1;
pub const ORIGAMI_BUTTON_RIGHT: u32 = 2;

/// One input event. Fields by `kind`:
/// KEY: `code` is an `origami_key`, `down`.
/// POINTER_MOVE: `x`, `y` are relative guest counts.
/// POINTER_AT: `head`, `x`, `y` in the head's pixels.
/// BUTTON: `code` is `ORIGAMI_BUTTON_*`, `down`.
/// WHEEL: `x` is notches, positive away from the user.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct origami_input_event {
    pub kind: u32,
    pub code: u32,
    pub down: u32,
    pub head: u32,
    pub x: i32,
    pub y: i32,
}

fn convert(e: &origami_input_event) -> Result<InputEvent, Fail> {
    Ok(match e.kind {
        ORIGAMI_INPUT_KEY => InputEvent::Key {
            key: keys::from_code(e.code)
                .ok_or_else(|| Fail(Status::InvalidArgument, "unknown key code".into()))?,
            down: e.down != 0,
        },
        ORIGAMI_INPUT_POINTER_MOVE => InputEvent::PointerMove { dx: e.x, dy: e.y },
        ORIGAMI_INPUT_POINTER_AT => InputEvent::PointerAt {
            head: e.head,
            x: e.x,
            y: e.y,
        },
        ORIGAMI_INPUT_BUTTON => InputEvent::Button {
            button: match e.code {
                ORIGAMI_BUTTON_LEFT => PointerButton::Left,
                ORIGAMI_BUTTON_MIDDLE => PointerButton::Middle,
                ORIGAMI_BUTTON_RIGHT => PointerButton::Right,
                _ => return fail(Status::InvalidArgument, "unknown button"),
            },
            down: e.down != 0,
        },
        ORIGAMI_INPUT_WHEEL => InputEvent::Wheel { notches: e.x },
        _ => return fail(Status::InvalidArgument, "unknown input kind"),
    })
}

/// Deliver `count` events to the guest, in order, as one batch. Never blocks.
#[no_mangle]
pub unsafe extern "C" fn origami_input_send(
    machine: *const origami_machine,
    events: *const origami_input_event,
    count: usize,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        if events.is_null() && count > 0 {
            return fail(Status::NullArgument, "events is null");
        }
        let events = if count == 0 {
            &[]
        } else {
            // SAFETY: `events` has `count` readable elements.
            unsafe { std::slice::from_raw_parts(events, count) }
        };
        let converted = events.iter().map(convert).collect::<Result<Vec<_>, _>>()?;
        m.machine.send_input(&converted)?;
        Ok(())
    })
}

/// Lift every key and button the guest believes is down; call when the
/// window loses focus. `released` (may be NULL) gets how many were lifted.
#[no_mangle]
pub unsafe extern "C" fn origami_input_release_all(
    machine: *const origami_machine,
    released: *mut u32,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        let n = m.machine.release_all_keys()?;
        if !released.is_null() {
            // SAFETY: `released` is writable when non-null.
            unsafe { *released = n };
        }
        Ok(())
    })
}

// --- views ------------------------------------------------------------------

/// The window system objects of a surface.
pub const ORIGAMI_SURFACE_COCOA_VIEW: u32 = 1;
pub const ORIGAMI_SURFACE_CA_METAL_LAYER: u32 = 2;
pub const ORIGAMI_SURFACE_WIN32_HWND: u32 = 3;
pub const ORIGAMI_SURFACE_WIN32_SWAPCHAIN_PANEL: u32 = 4;
pub const ORIGAMI_SURFACE_XLIB_WINDOW: u32 = 5;
pub const ORIGAMI_SURFACE_XCB_WINDOW: u32 = 6;
pub const ORIGAMI_SURFACE_WAYLAND_SURFACE: u32 = 7;
pub const ORIGAMI_SURFACE_WIN32_COMPOSITION_VISUAL: u32 = 8;

/// Where to draw. Fields by `kind`:
/// COCOA_VIEW: `window` is the `NSView*`.
/// CA_METAL_LAYER: `window` is the `CAMetalLayer*`.
/// WIN32_HWND: `window` is the `HWND`; its swap chain is opaque.
/// WIN32_SWAPCHAIN_PANEL: `window` is the panel's `ISwapChainPanelNative*`.
/// WIN32_COMPOSITION_VISUAL: `window` is an `IDCompositionVisual*`. Both blend
/// over what is behind them (a cube asks for premultiplied alpha).
/// XLIB_WINDOW: `window_id` is the `Window`, `display` the `Display*`,
/// `screen` the screen number.
/// XCB_WINDOW: `window_id` is the `xcb_window_t`, `display` the
/// `xcb_connection_t*`, `screen` the screen number.
/// WAYLAND_SURFACE: `window` is the `wl_surface*`, `display` the `wl_display*`.
/// The caller sets `size` to `sizeof(origami_surface)`.
#[repr(C)]
pub struct origami_surface {
    pub size: u32,
    pub kind: u32,
    pub window: *mut c_void,
    pub display: *mut c_void,
    pub window_id: u64,
    pub screen: i32,
}

/// The view: one head of one machine drawn into one window. Opaque to C.
pub struct origami_view {
    #[cfg(feature = "render")]
    inner: Mutex<present::Inner>,
}

/// The surface was presented, was skipped (hidden, zero size, no picture
/// yet), or was rebuilt and needs another call.
pub const ORIGAMI_DRAWN_SKIPPED: u32 = 0;
pub const ORIGAMI_DRAWN_PRESENTED: u32 = 1;
pub const ORIGAMI_DRAWN_AGAIN: u32 = 2;
/// The window is not on screen: the head was drawn offscreen, not presented.
pub const ORIGAMI_DRAWN_OCCLUDED: u32 = 3;

/// Draw `head` of `machine` into the window `surface` names, `width` by
/// `height` physical pixels at `scale` (2.0 on a Retina display). The view
/// holds the machine. The window must outlive the view: call
/// `origami_view_detach` first when it closes. Needs a GPU; returns `NoGpu`
/// without one.
#[no_mangle]
pub unsafe extern "C" fn origami_view_attach(
    machine: *const origami_machine,
    head: u32,
    surface: *const origami_surface,
    width: u32,
    height: u32,
    scale: f64,
    out: *mut *mut origami_view,
) -> origami_status {
    guard(|| {
        // SAFETY: argument contracts are in the header.
        let m = unsafe { machine_ref(machine)? };
        // SAFETY: `surface` is readable when non-null.
        let surface = unsafe { surface.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "surface is null".into()))?;
        if out.is_null() {
            return fail(Status::NullArgument, "out is null");
        }
        // SAFETY: the machine handle is live; the view keeps its own count.
        let machine = unsafe {
            Arc::increment_strong_count(machine);
            Arc::from_raw(machine)
        };
        let _ = m;
        let view = present::attach(machine, head, surface, width, height, scale)?;
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = Box::into_raw(Box::new(view)) };
        Ok(())
    })
}

/// The window changed size, in physical pixels.
#[no_mangle]
pub unsafe extern "C" fn origami_view_resize(
    view: *mut origami_view,
    width: u32,
    height: u32,
) -> origami_status {
    guard(|| present::with(view, |v| v.resize(Some((width, height)), None)))
}

/// The window moved to a display of another scale.
#[no_mangle]
pub unsafe extern "C" fn origami_view_set_scale(
    view: *mut origami_view,
    scale: f64,
) -> origami_status {
    guard(|| present::with(view, |v| v.resize(None, Some(scale))))
}

/// Whole-number scales only, nearest filtering; a window too small for 1x fits.
pub const ORIGAMI_SCALE_INTEGER: u32 = 0;
/// Fill the window keeping the aspect ratio, at any scale, sharp bilinear:
/// guest pixels stay flat and blend over one window pixel at their edges.
pub const ORIGAMI_SCALE_FIT: u32 = 1;

/// How the picture is scaled into the window. The default is
/// `ORIGAMI_SCALE_INTEGER`. Present again after changing it.
#[no_mangle]
pub unsafe extern "C" fn origami_view_set_scale_mode(
    view: *mut origami_view,
    mode: u32,
) -> origami_status {
    guard(|| present::with(view, |v| v.set_scale_mode(mode)))
}

/// Where the picture is drawn, in window physical pixels; guest x is
/// `(window x - x) / scale`.
#[repr(C)]
pub struct origami_view_layout {
    pub size: u32,
    pub reserved: u32,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Physical pixels per guest pixel.
    pub scale: f64,
}

/// Read where the picture is drawn under the current scale mode. Set `size` to
/// `sizeof`. `NotFound` before the first picture.
#[no_mangle]
pub unsafe extern "C" fn origami_view_get_layout(
    view: *mut origami_view,
    out: *mut origami_view_layout,
) -> origami_status {
    guard(|| {
        // SAFETY: `out` is writable when non-null.
        let out = unsafe { out.as_mut() }
            .ok_or_else(|| Fail(Status::NullArgument, "out is null".into()))?;
        if (out.size as usize) < std::mem::size_of::<origami_view_layout>() {
            return fail(Status::InvalidArgument, "out->size is too small");
        }
        let size = out.size;
        *out = present::with(view, |v| v.layout(size))?;
        Ok(())
    })
}

/// What the view's last present drew, for self-tests: the surface it drew into
/// and the picture's rectangle in it, in window physical pixels.
#[cfg(feature = "fake")]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct origami_view_last_draw {
    pub size: u32,
    pub reserved: u32,
    pub surface_width: u32,
    pub surface_height: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Pictures presented so far, so a test can tell a redraw from a stale read.
    pub presents: u64,
}

/// Read what the last present drew. Set `size` to `sizeof`. `NotFound` before
/// the first present.
#[cfg(feature = "fake")]
#[no_mangle]
pub unsafe extern "C" fn origami_view_get_last_draw(
    view: *mut origami_view,
    out: *mut origami_view_last_draw,
) -> origami_status {
    guard(|| {
        // SAFETY: `out` is writable when non-null.
        let out = unsafe { out.as_mut() }
            .ok_or_else(|| Fail(Status::NullArgument, "out is null".into()))?;
        if (out.size as usize) < std::mem::size_of::<origami_view_last_draw>() {
            return fail(Status::InvalidArgument, "out->size is too small");
        }
        let size = out.size;
        *out = present::with(view, |v| v.last_draw(size))?;
        Ok(())
    })
}

/// A hidden view draws nothing and takes nothing from the swapchain.
#[no_mangle]
pub unsafe extern "C" fn origami_view_set_visible(
    view: *mut origami_view,
    visible: u32,
) -> origami_status {
    guard(|| present::with(view, |v| v.set_visible(visible != 0)))
}

/// Take the head's newest picture (if any) and draw it. Call after a wake,
/// an expose, a resize, or an `ORIGAMI_DRAWN_AGAIN`. `drawn` (may be NULL)
/// gets `ORIGAMI_DRAWN_*`. No pixels pass through the caller.
#[no_mangle]
pub unsafe extern "C" fn origami_view_present(
    view: *mut origami_view,
    drawn: *mut u32,
) -> origami_status {
    guard(|| {
        let result = present::with(view, |v| v.present())?;
        if !drawn.is_null() {
            // SAFETY: `drawn` is writable when non-null.
            unsafe { *drawn = result };
        }
        Ok(())
    })
}

/// Copy what the view last drew (the picture and the guest's cursor, also
/// when the window was occluded) into `buf` as tight 4-byte pixels (blue,
/// green, red, unused), top row first, whatever the swapchain's format.
/// `width`, `height` and `stride` (bytes a row) get the picture's size; each
/// may be NULL. With `BufferTooSmall` they still say what is needed, so a call
/// with `cap` 0 sizes the buffer. `NotFound` before the first picture. It
/// draws again and waits for the GPU: for self-tests, not for every frame.
#[no_mangle]
pub unsafe extern "C" fn origami_view_readback(
    view: *mut origami_view,
    buf: *mut u8,
    cap: usize,
    width: *mut u32,
    height: *mut u32,
    stride: *mut u32,
) -> origami_status {
    guard(|| {
        let (w, h, pixels) = present::with(view, |v| v.readback())?;
        // SAFETY: each out pointer is writable when non-null.
        unsafe {
            for (p, v) in [(width, w), (height, h), (stride, w * 4)] {
                if !p.is_null() {
                    *p = v;
                }
            }
        }
        if pixels.len() > cap {
            return fail(Status::BufferTooSmall, "buf is too small for the picture");
        }
        if buf.is_null() {
            return fail(Status::NullArgument, "buf is null");
        }
        // SAFETY: `buf` is writable for `cap >= pixels.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), buf, pixels.len()) };
        Ok(())
    })
}

/// What a view has spent, in wall nanoseconds, since it was attached.
#[repr(C)]
pub struct origami_view_counters {
    pub size: u32,
    pub reserved: u32,
    /// Pictures drawn and presented.
    pub presents: u64,
    /// Taking the frame from the display client and updating the cursor.
    pub take_ns: u64,
    /// Acquiring the window's image, drawing the picture and the head into it.
    pub draw_ns: u64,
    /// Handing the image to the window system, which can make it large.
    pub present_ns: u64,
}

/// Read a view's counters.
#[no_mangle]
pub unsafe extern "C" fn origami_view_stats(
    view: *mut origami_view,
    out: *mut origami_view_counters,
) -> origami_status {
    guard(|| {
        // SAFETY: `out` is writable when non-null.
        let out = unsafe { out.as_mut() }
            .ok_or_else(|| Fail(Status::NullArgument, "out is null".into()))?;
        if (out.size as usize) < std::mem::size_of::<origami_view_counters>() {
            return fail(Status::InvalidArgument, "out->size is too small");
        }
        let size = out.size;
        *out = present::with(view, |v| Ok(v.stats(size)))?;
        Ok(())
    })
}

/// Destroy the view and let go of the window and the machine. Null is ignored.
#[no_mangle]
pub unsafe extern "C" fn origami_view_detach(view: *mut origami_view) {
    if !view.is_null() {
        // SAFETY: made by `origami_view_attach`, detached once.
        let view = unsafe { Box::from_raw(view) };
        let _ = catch_unwind(AssertUnwindSafe(move || drop(view)));
    }
}

#[cfg(feature = "render")]
pub(crate) mod present {
    use super::*;
    use origami_render::raw_window_handle::{
        AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle,
        WaylandDisplayHandle, WaylandWindowHandle, Win32WindowHandle, WindowsDisplayHandle,
        XcbDisplayHandle, XcbWindowHandle, XlibDisplayHandle, XlibWindowHandle,
    };
    use origami_render::{
        cursor_texture, Compose, Composition, CursorDraw, Drawn, Gpu, HeadTexture, Host,
        HostSurface, ScaleMode, View,
    };
    use std::ptr::NonNull;

    impl Drop for Inner {
        fn drop(&mut self) {
            // The display client gave this view the only copy of the picture; the next view of
            // the machine (a tab switched back to) starts from it.
            if let Some(t) = self.texture.take() {
                self.machine
                    .machine
                    .keep_picture(self.run, self.head, Box::new(t));
            }
        }
    }

    pub(super) struct Inner {
        machine: Arc<origami_machine>,
        head: u32,
        /// The run this view attached in; its picture is kept for that run only.
        run: u64,
        gpu: Gpu,
        view: View,
        compose: Compose,
        texture: Option<HeadTexture>,
        pixels: Vec<u8>,
        /// The guest cursor's image on the GPU and the generation it holds.
        cursor_tex: Option<(wgpu::Texture, u32)>,
        cursor: Option<f::CursorInfo>,
        take_ns: u64,
        size: (u32, u32),
        scale: f64,
        mode: ScaleMode,
        #[cfg(feature = "fake")]
        last: Option<origami_view_last_draw>,
    }

    pub(crate) fn gpu() -> Result<Gpu, Fail> {
        crate::engines::gpu::shared().map_err(|e| Fail(Status::NoGpu, e))
    }

    fn pointer(p: *mut c_void, what: &str) -> Result<NonNull<c_void>, Fail> {
        NonNull::new(p).ok_or_else(|| Fail(Status::NullArgument, format!("{what} is null")))
    }

    pub(crate) fn host(s: &origami_surface) -> Result<Host, Fail> {
        if (s.size as usize) < std::mem::size_of::<origami_surface>() {
            return fail(Status::InvalidArgument, "surface->size is too small");
        }
        let composition = match s.kind {
            ORIGAMI_SURFACE_WIN32_SWAPCHAIN_PANEL => Some(Composition::SwapChainPanel(s.window)),
            ORIGAMI_SURFACE_WIN32_COMPOSITION_VISUAL => Some(Composition::Visual(s.window)),
            _ => None,
        };
        if let Some(c) = composition {
            if !cfg!(windows) {
                return fail(Status::Unsupported, "a composition target needs Windows");
            }
            pointer(s.window, "window")?;
            return Ok(Host::Composition(c));
        }
        window_host(s).map(Host::Window)
    }

    fn window_host(s: &origami_surface) -> Result<HostSurface, Fail> {
        Ok(match s.kind {
            ORIGAMI_SURFACE_COCOA_VIEW | ORIGAMI_SURFACE_CA_METAL_LAYER => HostSurface {
                display: Some(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
                window: RawWindowHandle::AppKit(AppKitWindowHandle::new(pointer(
                    s.window, "window",
                )?)),
                metal_layer: s.kind == ORIGAMI_SURFACE_CA_METAL_LAYER,
            },
            ORIGAMI_SURFACE_WIN32_HWND => {
                let hwnd = std::num::NonZeroIsize::new(s.window as isize)
                    .ok_or_else(|| Fail(Status::NullArgument, "window is null".into()))?;
                HostSurface {
                    display: Some(RawDisplayHandle::Windows(WindowsDisplayHandle::new())),
                    window: RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)),
                    metal_layer: false,
                }
            }
            ORIGAMI_SURFACE_XLIB_WINDOW => HostSurface {
                display: Some(RawDisplayHandle::Xlib(XlibDisplayHandle::new(
                    NonNull::new(s.display),
                    s.screen,
                ))),
                window: RawWindowHandle::Xlib(XlibWindowHandle::new(
                    s.window_id as std::ffi::c_ulong,
                )),
                metal_layer: false,
            },
            ORIGAMI_SURFACE_XCB_WINDOW => HostSurface {
                display: Some(RawDisplayHandle::Xcb(XcbDisplayHandle::new(
                    NonNull::new(s.display),
                    s.screen,
                ))),
                window: RawWindowHandle::Xcb(XcbWindowHandle::new(
                    std::num::NonZeroU32::new(s.window_id as u32)
                        .ok_or_else(|| Fail(Status::NullArgument, "window_id is 0".into()))?,
                )),
                metal_layer: false,
            },
            ORIGAMI_SURFACE_WAYLAND_SURFACE => HostSurface {
                display: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    pointer(s.display, "display")?,
                ))),
                window: RawWindowHandle::Wayland(WaylandWindowHandle::new(pointer(
                    s.window, "window",
                )?)),
                metal_layer: false,
            },
            _ => return fail(Status::InvalidArgument, "unknown surface kind"),
        })
    }

    pub(super) unsafe fn attach(
        machine: Arc<origami_machine>,
        head: u32,
        surface: &origami_surface,
        width: u32,
        height: u32,
        scale: f64,
    ) -> Result<origami_view, Fail> {
        let host = host(surface)?;
        let gpu = gpu()?;
        // SAFETY: the caller promises the handles stay valid until detach.
        let view = unsafe { View::attach_to(&gpu, host, width, height, scale, false) }
            .map_err(|e| Fail(Status::NoGpu, e))?;
        Ok(origami_view {
            inner: Mutex::new(Inner {
                run: machine.machine.run_serial(),
                machine,
                head,
                compose: Compose::new(&gpu),
                gpu,
                view,
                texture: None,
                pixels: Vec::new(),
                cursor_tex: None,
                cursor: None,
                take_ns: 0,
                size: (width, height),
                scale,
                mode: ScaleMode::Integer,
                #[cfg(feature = "fake")]
                last: None,
            }),
        })
    }

    pub(super) unsafe fn with<T>(
        view: *mut origami_view,
        body: impl FnOnce(&mut Inner) -> Result<T, Fail>,
    ) -> Result<T, Fail> {
        // SAFETY: a non-null pointer came from `origami_view_attach`.
        let view = unsafe { view.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "view is null".into()))?;
        body(&mut view.inner.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// The guest cursor to draw, when it is on and its image is the current one.
    fn cursor_draw<'a>(
        cursor: &Option<f::CursorInfo>,
        tex: &'a Option<(wgpu::Texture, u32)>,
    ) -> Option<CursorDraw<'a>> {
        match (cursor, tex) {
            (Some(c), Some((tex, gen))) if c.on && *gen == c.gen => Some(CursorDraw {
                texture: tex,
                x: c.x - c.hot_x,
                y: c.y - c.hot_y,
                width: c.width,
                height: c.height,
                gen: c.gen,
            }),
            _ => None,
        }
    }

    impl Inner {
        pub(super) fn resize(
            &mut self,
            size: Option<(u32, u32)>,
            scale: Option<f64>,
        ) -> Result<(), Fail> {
            self.size = size.unwrap_or(self.size);
            self.scale = scale.unwrap_or(self.scale);
            self.view
                .resize(&self.gpu, self.size.0, self.size.1, self.scale);
            Ok(())
        }

        pub(super) fn set_scale_mode(&mut self, mode: u32) -> Result<(), Fail> {
            self.mode = match mode {
                ORIGAMI_SCALE_INTEGER => ScaleMode::Integer,
                ORIGAMI_SCALE_FIT => ScaleMode::Fit,
                _ => return fail(Status::InvalidArgument, "unknown scale mode"),
            };
            Ok(())
        }

        pub(super) fn layout(&self, size: u32) -> Result<origami_view_layout, Fail> {
            let Some(texture) = &self.texture else {
                return fail(Status::NotFound, "no picture has been drawn yet");
            };
            let l = self.view.layout((texture.width, texture.height), self.mode);
            Ok(origami_view_layout {
                size,
                reserved: 0,
                x: f64::from(l.x),
                y: f64::from(l.y),
                width: f64::from(l.width),
                height: f64::from(l.height),
                scale: f64::from(l.scale),
            })
        }

        #[cfg(feature = "fake")]
        pub(super) fn last_draw(&self, size: u32) -> Result<origami_view_last_draw, Fail> {
            let Some(last) = &self.last else {
                return fail(Status::NotFound, "nothing has been presented yet");
            };
            Ok(origami_view_last_draw { size, ..*last })
        }

        pub(super) fn set_visible(&mut self, visible: bool) -> Result<(), Fail> {
            self.view.set_visible(visible);
            self.machine.machine.set_visible(visible)?;
            Ok(())
        }

        /// Read the guest's cursor and keep its image on the GPU.
        fn refresh_cursor(&mut self) -> Result<(), Fail> {
            let known = self.cursor_tex.as_ref().map_or(u32::MAX, |t| t.1);
            let Some(mut info) = self.machine.machine.cursor(self.head as usize, known)? else {
                self.cursor = None;
                return Ok(());
            };
            if let Some(image) = info.image.take() {
                let tex = self
                    .cursor_tex
                    .take()
                    .map_or_else(|| cursor_texture(&self.gpu), |t| t.0);
                self.gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &image,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(info.width * 4),
                        rows_per_image: Some(info.height),
                    },
                    wgpu::Extent3d {
                        width: info.width,
                        height: info.height,
                        depth_or_array_layers: 1,
                    },
                );
                self.cursor_tex = Some((tex, info.gen));
            }
            self.cursor = Some(info);
            Ok(())
        }

        pub(super) fn stats(&self, size: u32) -> origami_view_counters {
            origami_view_counters {
                size,
                reserved: 0,
                presents: self.view.presents,
                take_ns: self.take_ns,
                draw_ns: self.view.draw_ns,
                present_ns: self.view.present_ns,
            }
        }

        /// Draw the newest picture the view took again, offscreen, and copy it
        /// out as tight Bgra8 rows: `(width, height, pixels)`.
        pub(super) fn readback(&self) -> Result<(u32, u32, Vec<u8>), Fail> {
            let Some(texture) = &self.texture else {
                return fail(Status::NotFound, "no picture has been drawn yet");
            };
            let draw = cursor_draw(&self.cursor, &self.cursor_tex);
            let (w, h) = self.view.size();
            let pixels = self
                .view
                .readback(&self.gpu, texture, self.mode, draw.as_ref())
                .map_err(|e| Fail(Status::Internal, e))?;
            Ok((w, h, pixels))
        }

        /// A view made after another one starts from the picture that one left, if it is
        /// still the size of the head.
        fn take_kept_picture(&mut self) {
            if self.texture.is_some() {
                return;
            }
            let Some(kept) = self.machine.machine.take_kept_picture(self.head) else {
                return;
            };
            let Ok(t) = kept.downcast::<HeadTexture>() else {
                return;
            };
            let heads = self.machine.machine.heads();
            if heads
                .get(self.head as usize)
                .is_some_and(|h| h.active && (h.width, h.height) == (t.width, t.height))
            {
                self.texture = Some(*t);
            }
        }

        pub(super) fn present(&mut self) -> Result<u32, Fail> {
            let start = std::time::Instant::now();
            // Changes pile up in the display while hidden; the first present
            // after showing takes them.
            if !self.view.visible() {
                return Ok(ORIGAMI_DRAWN_SKIPPED);
            }
            // Before the take: an error here must not drop a frame whose
            // changed lines the producer will not send again.
            self.refresh_cursor()?;
            self.take_kept_picture();
            let mut pending = None;
            let mut fresh = false;
            match self
                .machine
                .machine
                .take_frame(self.head as usize, &mut self.pixels)?
            {
                Some(Frame::Rows(update)) => {
                    fresh = true;
                    self.texture = Some(HeadTexture::apply(
                        &self.gpu,
                        self.texture.take(),
                        &update,
                        &self.pixels,
                    ));
                }
                Some(Frame::Imported(imported)) => {
                    fresh = true;
                    self.texture = Some(HeadTexture::prepare(
                        &self.gpu,
                        self.texture.take(),
                        &imported,
                    ));
                    pending = Some(imported);
                }
                None => {}
            }
            self.take_ns += start.elapsed().as_nanos() as u64;
            let Some(texture) = &self.texture else {
                return Ok(ORIGAMI_DRAWN_SKIPPED);
            };
            let draw = cursor_draw(&self.cursor, &self.cursor_tex);
            // The picture's rows and the window's pass go in one submit.
            let (gpu, compose) = (&self.gpu, &self.compose);
            let wanted = self.machine.reflection(self.head);
            if let Some(r) = &wanted {
                r.set_source(&texture.texture);
            }
            let reflect = wanted.filter(|r| fresh && r.take_due());
            let mut rows = None;
            let drawn = self
                .view
                .draw_with(gpu, texture, self.mode, draw.as_ref(), |enc| {
                    if let Some(frame) = &pending {
                        rows = Some(texture.encode_imported(gpu, compose, enc, frame));
                    }
                    if let Some(r) = &reflect {
                        r.encode(gpu, enc, &texture.texture);
                    }
                })
                .map_err(|e| Fail(Status::Internal, e))?;
            if let Some(r) = &reflect {
                r.done();
            }
            if let (Some(rows), Some(frame), Some(texture)) =
                (rows, &pending, self.texture.as_mut())
            {
                texture.note_imported(gpu, rows, frame);
            }
            // An occluded window is drawn offscreen at its real size, which is what a runner's window gets.
            #[cfg(feature = "fake")]
            if matches!(drawn, Drawn::Presented | Drawn::Occluded) {
                if let Some(t) = &self.texture {
                    let l = self.view.layout((t.width, t.height), self.mode);
                    let (w, h) = self.view.size();
                    self.last = Some(origami_view_last_draw {
                        size: 0,
                        reserved: 0,
                        surface_width: w,
                        surface_height: h,
                        x: l.x.round() as i32,
                        y: l.y.round() as i32,
                        width: l.width.round() as u32,
                        height: l.height.round() as u32,
                        presents: self.view.presents,
                    });
                }
            }
            Ok(match drawn {
                Drawn::Presented => ORIGAMI_DRAWN_PRESENTED,
                Drawn::Skipped => ORIGAMI_DRAWN_SKIPPED,
                Drawn::Reconfigured => ORIGAMI_DRAWN_AGAIN,
                Drawn::Occluded => ORIGAMI_DRAWN_OCCLUDED,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn surface(kind: u32) -> origami_surface {
            origami_surface {
                size: std::mem::size_of::<origami_surface>() as u32,
                kind,
                window: std::ptr::dangling_mut::<c_void>(),
                display: std::ptr::null_mut(),
                window_id: 0,
                screen: 0,
            }
        }

        // wgpu refuses a surface with no display handle on any platform.
        #[test]
        fn cocoa_and_win32_surfaces_carry_a_display_handle() {
            for kind in [ORIGAMI_SURFACE_COCOA_VIEW, ORIGAMI_SURFACE_WIN32_HWND] {
                let host = window_host(&surface(kind)).ok().unwrap();
                assert!(host.display.is_some(), "kind {kind}");
                assert!(!host.metal_layer);
            }
            let layer = window_host(&surface(ORIGAMI_SURFACE_CA_METAL_LAYER))
                .ok()
                .unwrap();
            assert!(layer.display.is_some() && layer.metal_layer);
        }
    }
}

#[cfg(not(feature = "render"))]
mod present {
    use super::*;

    pub(super) struct Inner;

    impl Inner {
        pub(super) fn resize(&mut self, _: Option<(u32, u32)>, _: Option<f64>) -> Result<(), Fail> {
            unreachable!()
        }
        pub(super) fn set_visible(&mut self, _: bool) -> Result<(), Fail> {
            unreachable!()
        }
        pub(super) fn set_scale_mode(&mut self, _: u32) -> Result<(), Fail> {
            unreachable!()
        }
        pub(super) fn layout(&self, _: u32) -> Result<origami_view_layout, Fail> {
            unreachable!()
        }
        #[cfg(feature = "fake")]
        pub(super) fn last_draw(&self, _: u32) -> Result<origami_view_last_draw, Fail> {
            unreachable!()
        }
        pub(super) fn present(&mut self) -> Result<u32, Fail> {
            unreachable!()
        }
        pub(super) fn readback(&self) -> Result<(u32, u32, Vec<u8>), Fail> {
            unreachable!()
        }
        pub(super) fn stats(&self, _: u32) -> origami_view_counters {
            unreachable!()
        }
    }

    pub(super) unsafe fn attach(
        _: Arc<origami_machine>,
        _: u32,
        _: &origami_surface,
        _: u32,
        _: u32,
        _: f64,
    ) -> Result<origami_view, Fail> {
        fail(Status::Unsupported, "this build has no renderer")
    }

    pub(super) unsafe fn with<T>(
        _: *mut origami_view,
        _: impl FnOnce(&mut Inner) -> Result<T, Fail>,
    ) -> Result<T, Fail> {
        fail(Status::Unsupported, "this build has no renderer")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exported_codes_match_the_internal_ones() {
        for (status, code) in [
            (Status::NullArgument, ORIGAMI_ERR_NULL_ARGUMENT),
            (Status::InvalidArgument, ORIGAMI_ERR_INVALID_ARGUMENT),
            (Status::NotRunning, ORIGAMI_ERR_NOT_RUNNING),
            (Status::BufferTooSmall, ORIGAMI_ERR_BUFFER_TOO_SMALL),
            (Status::Unsupported, ORIGAMI_ERR_UNSUPPORTED),
            (Status::NoGpu, ORIGAMI_ERR_NO_GPU),
            (Status::Panic, ORIGAMI_ERR_PANIC),
            (Status::Engine, ORIGAMI_ERR_ENGINE),
            (Status::NotFound, ORIGAMI_ERR_NOT_FOUND),
            (Status::Busy, ORIGAMI_ERR_BUSY),
            (Status::Io, ORIGAMI_ERR_IO),
            (Status::Internal, ORIGAMI_ERR_INTERNAL),
        ] {
            assert_eq!(status.code(), code, "{status:?}");
        }
    }

    #[test]
    fn a_panic_becomes_a_status_and_a_message() {
        let status = guard(|| panic!("boom"));
        assert_eq!(status, ORIGAMI_ERR_PANIC);
        LAST_ERROR.with(|last| assert!(last.borrow().contains("panic")));
    }
}
