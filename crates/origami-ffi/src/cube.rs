//! The C frame plane's cube view: the SGI cube drawn live on the GPU into a
//! host window, on the device the screen view uses. Declared in
//! `include/origami_frame.h`, which cbindgen generates from this file.
//!
//! The host owns the clock. It calls `origami_cube_present` on each display
//! refresh while that reports the cube is moving, stops when it reports rest,
//! and calls it once after any other call that changes the cube. Nothing here
//! spawns a thread or draws on its own.

#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]

use crate::frame::{
    fail, guard, machine_ref, origami_machine, origami_status, origami_surface,
    origami_view_counters, present, Fail, Status, ORIGAMI_DRAWN_AGAIN, ORIGAMI_DRAWN_OCCLUDED,
    ORIGAMI_DRAWN_PRESENTED, ORIGAMI_DRAWN_SKIPPED,
};
use origami_render::{scenes, Backdrop, CubeView, Drawn, Env, Gpu, Pointer, Reflection, Style};
use std::sync::{Arc, Mutex};

/// What the chrome reflects: the studio, a photographed scene
/// (`origami_cube_style.scene` names which) or the screen of the machine
/// given to `origami_cube_set_machine`.
pub const ORIGAMI_CUBE_ENV_STUDIO: u32 = 0;
pub const ORIGAMI_CUBE_ENV_SCENE: u32 = 1;
pub const ORIGAMI_CUBE_ENV_MACHINE: u32 = 2;
/// The classic chrome horizon: dark ground, bright band, blue sky.
pub const ORIGAMI_CUBE_ENV_HORIZON: u32 = 3;

/// `origami_cube_scene_text` fields.
pub const ORIGAMI_CUBE_SCENE_ID: u32 = 0;
pub const ORIGAMI_CUBE_SCENE_TITLE: u32 = 1;
pub const ORIGAMI_CUBE_SCENE_CREDIT: u32 = 2;
pub const ORIGAMI_CUBE_SCENE_SOURCE: u32 = 3;

/// `origami_cube_pointer` phases.
pub const ORIGAMI_CUBE_POINTER_PRESS: u32 = 0;
pub const ORIGAMI_CUBE_POINTER_MOVE: u32 = 1;
pub const ORIGAMI_CUBE_POINTER_RELEASE: u32 = 2;
/// Eases the cube back to the mark pose.
pub const ORIGAMI_CUBE_POINTER_DOUBLE_CLICK: u32 = 3;

/// `origami_cube_style.backdrop` values.
pub const ORIGAMI_CUBE_BACKDROP_NONE: u32 = 0;
pub const ORIGAMI_CUBE_BACKDROP_BAND: u32 = 1;

/// A colour field with this value means none.
pub const ORIGAMI_CUBE_NO_COLOUR: u32 = 0xffff_ffff;

/// How the cube looks and moves. Colours are `0xRRGGBB`; the caller sets
/// `size` to `sizeof(origami_cube_style)`.
/// - `env`: `ORIGAMI_CUBE_ENV_*`. The machine's screen shows the studio until
///   its first picture has been copied.
/// - `scene`: for `ORIGAMI_CUBE_ENV_SCENE`, an index below
///   `origami_cube_scene_count`; `origami_cube_scene_for_time` gives About's.
/// - `horizon_rgb`: the studio's horizon band colour (a machine's chassis
///   colour on the start screen), or `ORIGAMI_CUBE_NO_COLOUR`.
/// - `granite_light_rgb`, `granite_dark_rgb`: the band's top and bottom tones
///   where there is no scene to take them from (the studio, a machine's
///   screen); a scene colours its own.
/// - `light_look`: the lighter chrome studio, for a light window.
/// - `spin`: turn at 10 rpm; without it the cube is at rest until dragged.
/// - `reduce_motion`: draw the still pose and never spin; a drag still turns
///   it, with no coasting and no easing: it is back at the pose on release.
/// - `backdrop`: `_NONE` leaves the view transparent; `_BAND` fills it with a
///   scene gradient and floor shadow. `surround_rgb` fills where no alpha.
/// - `settle`: after a drag the cube coasts, then eases back to the still pose.
/// - `fill_percent`: the cube's width as a percentage of the view's shorter
///   side; 0 for the default, 90.
#[repr(C)]
pub struct origami_cube_style {
    pub size: u32,
    pub env: u32,
    pub scene: u32,
    pub horizon_rgb: u32,
    pub granite_light_rgb: u32,
    pub granite_dark_rgb: u32,
    pub light_look: u32,
    pub spin: u32,
    pub reduce_motion: u32,
    pub backdrop: u32,
    pub surround_rgb: u32,
    pub settle: u32,
    pub fill_percent: u32,
}

/// The cube view: one cube drawn into one window. Opaque to C.
pub struct origami_cube {
    inner: Mutex<Inner>,
}

struct Inner {
    gpu: Gpu,
    view: CubeView,
}

fn with<T>(
    cube: *mut origami_cube,
    body: impl FnOnce(&mut Inner) -> Result<T, Fail>,
) -> Result<T, Fail> {
    // SAFETY: a non-null pointer came from `origami_cube_attach`.
    let cube = unsafe { cube.as_ref() }
        .ok_or_else(|| Fail(Status::NullArgument, "cube is null".into()))?;
    body(&mut cube.inner.lock().unwrap_or_else(|e| e.into_inner()))
}

/// [`with`] for a body that cannot fail.
fn act(cube: *mut origami_cube, body: impl FnOnce(&mut Inner)) -> Result<(), Fail> {
    with(cube, |c| {
        body(c);
        Ok(())
    })
}

fn colour(hex: u32) -> Option<[f32; 3]> {
    (hex != ORIGAMI_CUBE_NO_COLOUR).then(|| [16, 8, 0].map(|s| ((hex >> s) & 0xff) as f32 / 255.0))
}

/// Draw a cube into the window `surface` names, `width` by `height` physical
/// pixels at `scale` (2.0 on a Retina display). The cube fills 90% of the
/// shorter side unless the style says otherwise, so size the view for it. The
/// window must outlive the cube: call `origami_cube_detach` first when it
/// closes. Needs a GPU; returns `NoGpu` without one. The cube starts in the
/// studio with no spin; call `origami_cube_set_style`. Call a cube's functions
/// from one thread at a time: they wait for the GPU.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_attach(
    surface: *const origami_surface,
    width: u32,
    height: u32,
    scale: f64,
    out: *mut *mut origami_cube,
) -> origami_status {
    guard(|| {
        if !(scale.is_finite() && scale > 0.0) {
            return fail(Status::InvalidArgument, "scale is not positive");
        }
        // SAFETY: `surface` is readable when non-null.
        let surface = unsafe { surface.as_ref() }
            .ok_or_else(|| Fail(Status::NullArgument, "surface is null".into()))?;
        if out.is_null() {
            return fail(Status::NullArgument, "out is null");
        }
        let host = present::host(surface)?;
        let gpu = present::gpu()?;
        // SAFETY: the caller promises the handles stay valid until detach.
        let view = unsafe { CubeView::attach(&gpu, host, width, height, scale) }
            .map_err(|e| Fail(Status::NoGpu, e))?;
        let cube = origami_cube {
            inner: Mutex::new(Inner { gpu, view }),
        };
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = Box::into_raw(Box::new(cube)) };
        Ok(())
    })
}

/// A cube with no window, for a toolkit that cannot host a native surface
/// (GTK): `width` by `height` physical pixels at `scale`. Each
/// `origami_cube_present` that draws renders into memory, and
/// `origami_cube_readback` then returns that frame without drawing again, for
/// the host to wrap in its own texture. Everything else works as for an
/// attached cube. The copy waits for the GPU, so call from one thread.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_attach_offscreen(
    width: u32,
    height: u32,
    scale: f64,
    out: *mut *mut origami_cube,
) -> origami_status {
    guard(|| {
        if !(scale.is_finite() && scale > 0.0) {
            return fail(Status::InvalidArgument, "scale is not positive");
        }
        if out.is_null() {
            return fail(Status::NullArgument, "out is null");
        }
        let gpu = present::gpu()?;
        let view = CubeView::offscreen(&gpu, width, height, scale)
            .map_err(|e| Fail(Status::InvalidArgument, e))?;
        let cube = origami_cube {
            inner: Mutex::new(Inner { gpu, view }),
        };
        // SAFETY: `out` is non-null and writable.
        unsafe { *out = Box::into_raw(Box::new(cube)) };
        Ok(())
    })
}

/// The window changed size (physical pixels) or moved to a display of another
/// scale. Call `origami_cube_present` after it.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_resize(
    cube: *mut origami_cube,
    width: u32,
    height: u32,
    scale: f64,
) -> origami_status {
    guard(|| {
        if !(scale.is_finite() && scale > 0.0) {
            return fail(Status::InvalidArgument, "scale is not positive");
        }
        act(cube, |c| c.view.resize(&c.gpu, width, height, scale))
    })
}

/// A hidden cube draws nothing and takes nothing from the swapchain.
/// Hide it when its window is hidden, covered or minimised, and when the first
/// picture of the machine arrives. Showing it again needs an
/// `origami_cube_present`.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_set_visible(
    cube: *mut origami_cube,
    visible: u32,
) -> origami_status {
    guard(|| act(cube, |c| c.view.set_visible(visible != 0)))
}

/// Set how the cube looks and moves. Call `origami_cube_present` after it.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_set_style(
    cube: *mut origami_cube,
    style: *const origami_cube_style,
) -> origami_status {
    guard(|| {
        if style.is_null() {
            return fail(Status::NullArgument, "style is null");
        }
        // A caller built before the newest fields passes a shorter `size`:
        // copy only that many bytes, and the rest stay zero.
        // SAFETY: the first field is readable, and `size` says how much is.
        let size = unsafe { style.cast::<u32>().read_unaligned() } as usize;
        let full = std::mem::size_of::<origami_cube_style>();
        if size < std::mem::offset_of!(origami_cube_style, backdrop) {
            return fail(Status::InvalidArgument, "style->size is too small");
        }
        // SAFETY: all fields are plain integers, so zero is a valid value.
        let mut s: origami_cube_style = unsafe { std::mem::zeroed() };
        // SAFETY: `style` is readable for `size` bytes, `s` for `full`.
        unsafe {
            std::ptr::copy_nonoverlapping(
                style.cast::<u8>(),
                std::ptr::from_mut(&mut s).cast::<u8>(),
                size.min(full),
            )
        };
        // The backdrop and surround fields arrived together.
        let rounded = size >= std::mem::offset_of!(origami_cube_style, surround_rgb) + 4;
        let env = match s.env {
            ORIGAMI_CUBE_ENV_STUDIO => Env::Studio,
            ORIGAMI_CUBE_ENV_SCENE if (s.scene as usize) < scenes::count() => {
                Env::Scene(s.scene as usize)
            }
            ORIGAMI_CUBE_ENV_SCENE => return fail(Status::InvalidArgument, "unknown scene"),
            ORIGAMI_CUBE_ENV_MACHINE => Env::Screen,
            ORIGAMI_CUBE_ENV_HORIZON => Env::Horizon,
            _ => return fail(Status::InvalidArgument, "unknown env"),
        };
        let tone = |hex| {
            colour(hex).ok_or_else(|| Fail(Status::InvalidArgument, "a band tone is none".into()))
        };
        let style = Style {
            env,
            horizon: colour(s.horizon_rgb),
            tones: [tone(s.granite_light_rgb)?, tone(s.granite_dark_rgb)?],
            light_look: s.light_look != 0,
            spin: s.spin != 0,
            reduce_motion: s.reduce_motion != 0,
            settle: s.settle != 0,
            backdrop: if rounded && s.backdrop == ORIGAMI_CUBE_BACKDROP_NONE {
                Backdrop::None
            } else {
                Backdrop::Band
            },
            surround: colour(s.surround_rgb)
                .filter(|_| rounded)
                .unwrap_or([0.0; 3]),
            fill: match s.fill_percent {
                0 => Style::default().fill,
                p => (p.min(100) as f32) / 100.0,
            },
        };
        act(cube, |c| c.view.set_style(&c.gpu, style))
    })
}

/// A pointer event at `x`, `y` in points (logical pixels) from the view's top
/// left, `phase` an `ORIGAMI_CUBE_POINTER_*` and `time_ms` from any steady
/// clock. Dragging tumbles the cube; releasing a fast drag flings it. Call
/// `origami_cube_present` after it.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_pointer(
    cube: *mut origami_cube,
    phase: u32,
    x: f32,
    y: f32,
    time_ms: u64,
) -> origami_status {
    guard(|| {
        if !(x.is_finite() && y.is_finite()) {
            return fail(Status::InvalidArgument, "the position is not finite");
        }
        let phase = match phase {
            ORIGAMI_CUBE_POINTER_PRESS => Pointer::Press,
            ORIGAMI_CUBE_POINTER_MOVE => Pointer::Move,
            ORIGAMI_CUBE_POINTER_RELEASE => Pointer::Release,
            ORIGAMI_CUBE_POINTER_DOUBLE_CLICK => Pointer::DoubleClick,
            _ => return fail(Status::InvalidArgument, "unknown pointer phase"),
        };
        act(cube, |c| c.view.pointer(phase, x, y, time_ms))
    })
}

/// Step to the next scene in the manifest's `about.order` (time of day), fading
/// from the current one.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_next_scene(cube: *mut origami_cube) -> origami_status {
    guard(|| act(cube, |c| c.view.step_scene(&c.gpu)))
}

/// The index of the scene on show, for `origami_cube_scene_text`; -1 for the
/// studio, a machine's screen or a null `cube`.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_current_scene(cube: *mut origami_cube) -> i32 {
    let mut scene = -1;
    let _ = guard(|| {
        scene = with(cube, |c| Ok(c.view.current_scene()))?.map_or(-1, |s| s as i32);
        Ok(())
    });
    scene
}

/// The scene About shows at the host's local time (`hour` 0 to 23, `minute` 0
/// to 59): the first scene in `scenes.json` whose hours hold it.
#[no_mangle]
pub extern "C" fn origami_cube_scene_for_time(hour: u32, minute: u32) -> u32 {
    scenes::for_time(hour.min(23), minute.min(59)) as u32
}

/// How many scenes there are: photographs embedded from
/// `resources/identity/scenes/scenes.json`.
#[no_mangle]
pub extern "C" fn origami_cube_scene_count() -> u32 {
    scenes::count() as u32
}

/// The index of the scene whose id is `id`, or -1.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_scene_find(id: *const std::ffi::c_char) -> i32 {
    if id.is_null() {
        return -1;
    }
    // SAFETY: the caller promises a NUL terminated string.
    let id = unsafe { std::ffi::CStr::from_ptr(id) };
    id.to_str()
        .ok()
        .and_then(scenes::find)
        .map_or(-1, |i| i as i32)
}

/// Copy a scene's text (`ORIGAMI_CUBE_SCENE_*`: its id, title, credit line or
/// source URL) into `buf`, NUL terminated and truncated to fit, and return its
/// full length without the NUL; 0 for an unknown scene or field. The credit
/// line is what About's acknowledgements must show.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_scene_text(
    index: u32,
    field: u32,
    buf: *mut std::ffi::c_char,
    cap: usize,
) -> usize {
    let Some(scene) = scenes::SCENES.get(index as usize) else {
        return 0;
    };
    let text = match field {
        ORIGAMI_CUBE_SCENE_ID => scene.id,
        ORIGAMI_CUBE_SCENE_TITLE => scene.title,
        ORIGAMI_CUBE_SCENE_CREDIT => scene.credit,
        ORIGAMI_CUBE_SCENE_SOURCE => scene.source,
        _ => return 0,
    };
    if !buf.is_null() && cap > 0 {
        let n = text.len().min(cap - 1);
        // SAFETY: `buf` is writable for `cap` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), buf.cast::<u8>(), n);
            *buf.add(n) = 0;
        }
    }
    text.len()
}

/// Reflect `head` of `machine` when the style's env is
/// `ORIGAMI_CUBE_ENV_MACHINE`: the view presenting that head copies its
/// picture into a 256 by 256 texture on the GPU at most ten times a second.
/// The machine needs a presenting `origami_view` (call `origami_view_present`
/// once after this so a still head is copied); one cube per head. A NULL
/// `machine` stops the copying. Call `origami_cube_present` after it.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_set_machine(
    cube: *mut origami_cube,
    machine: *const origami_machine,
    head: u32,
) -> origami_status {
    guard(|| {
        let reflection = if machine.is_null() {
            None
        } else {
            // SAFETY: a non-null machine handle is live (see the header).
            let m = unsafe { machine_ref(machine)? };
            let r = with(cube, |c| Ok(Reflection::new(&c.gpu)))?;
            *m.reflection.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((head, Arc::downgrade(&r)));
            Some(r)
        };
        act(cube, |c| c.view.set_reflection(reflection))
    })
}

/// Draw a frame when the cube changed or is moving, and present it. `drawn`
/// (may be NULL) gets `ORIGAMI_DRAWN_*`. `moving` (may be NULL) gets 1 while
/// the cube is turning or settling: call again on the next display refresh.
/// It gets 0 at rest: stop your timer and draw nothing more until you change
/// the cube. Frames come at most 60 times a second whatever the refresh rate
/// (30 while only the calm spin turns it), and the cube turns by wall time,
/// so a skipped refresh never slows it.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_present(
    cube: *mut origami_cube,
    drawn: *mut u32,
    moving: *mut u32,
) -> origami_status {
    guard(|| {
        let (d, m) = with(cube, |c| {
            c.view.draw(&c.gpu).map_err(|e| Fail(Status::Internal, e))
        })?;
        let d = match d {
            Drawn::Presented => ORIGAMI_DRAWN_PRESENTED,
            Drawn::Skipped => ORIGAMI_DRAWN_SKIPPED,
            Drawn::Reconfigured => ORIGAMI_DRAWN_AGAIN,
            Drawn::Occluded => ORIGAMI_DRAWN_OCCLUDED,
        };
        // SAFETY: each out pointer is writable when non-null.
        unsafe {
            if !drawn.is_null() {
                *drawn = d;
            }
            if !moving.is_null() {
                *moving = u32::from(m);
            }
        }
        Ok(())
    })
}

/// Copy the cube into `buf` as tight 4-byte pixels (blue, green, red,
/// unused), top row first. A window's cube is drawn again offscreen as it is
/// now, waiting for the GPU: for self-tests. An offscreen cube returns the
/// frame `origami_cube_present` last drew (`NotFound` before the first).
/// Sizes and errors as `origami_view_readback`.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_readback(
    cube: *mut origami_cube,
    buf: *mut u8,
    cap: usize,
    width: *mut u32,
    height: *mut u32,
    stride: *mut u32,
) -> origami_status {
    guard(|| {
        let (w, h, pixels) = with(cube, |c| {
            c.view
                .readback(&c.gpu)
                .map_err(|e| Fail(Status::Internal, e))
        })?;
        // SAFETY: each out pointer is writable when non-null.
        unsafe {
            for (p, v) in [(width, w), (height, h), (stride, w * 4)] {
                if !p.is_null() {
                    *p = v;
                }
            }
        }
        if pixels.is_empty() {
            return fail(Status::NotFound, "no frame has been drawn yet");
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

/// What a cube has spent since it was attached: `presents`, `draw_ns` and
/// `present_ns` as for a view; `take_ns` stays 0.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_stats(
    cube: *mut origami_cube,
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
        *out = with(cube, |c| {
            let (presents, draw_ns, present_ns) = c.view.counters();
            Ok(origami_view_counters {
                size,
                reserved: 0,
                presents,
                take_ns: 0,
                draw_ns,
                present_ns,
            })
        })?;
        Ok(())
    })
}

/// Destroy the cube and let go of the window. Null is ignored.
#[no_mangle]
pub unsafe extern "C" fn origami_cube_detach(cube: *mut origami_cube) {
    if !cube.is_null() {
        // SAFETY: made by `origami_cube_attach`, detached once.
        let cube = unsafe { Box::from_raw(cube) };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(cube)));
    }
}
