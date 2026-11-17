//! The cube view through the C frame plane, in an X11 window: each
//! environment is drawn and read back, the pointer, Alt-click and the
//! machine reflection act, and CPU time is measured. Ignored: needs a Vulkan
//! device and an X server (`DISPLAY`). Run with
//! `cargo test -p origami-ffi --release --features fake --test cube_view -- --ignored --nocapture`;
//! `ORIGAMI_CUBE_SHOTS` names a directory for PNG screenshots and
//! `ORIGAMI_CUBE_SECS` the seconds the measurement runs (default 10). The
//! numbers it prints start with `MEASURE`.
#![cfg(all(target_os = "linux", feature = "fake", feature = "render"))]
#![allow(clippy::unnecessary_cast)]

mod boot_rig;
mod common;

use boot_rig::*;
use common::*;
use origami_ffi::*;
use std::ffi::CString;
use std::path::PathBuf;
use std::ptr;
use std::time::{Duration, Instant};

const TEAL: u32 = 0x2a8f98;
const DARK: [u32; 2] = [0x282a30, 0x1f2126];
const TINTED: [u32; 2] = [0x1e3c40, 0x182e32];
const LIGHT: [u32; 2] = [0xe2e4e8, 0xd6d8dd];

struct Window {
    dpy: *mut std::ffi::c_void,
    id: std::ffi::c_ulong,
    screen: i32,
}

impl Window {
    fn new(w: u32, h: u32) -> Window {
        // SAFETY: Xlib calls on a display we open and keep.
        unsafe {
            let dpy = XOpenDisplay(ptr::null());
            assert!(!dpy.is_null(), "no X display; set DISPLAY");
            let screen = XDefaultScreen(dpy);
            let id = XCreateSimpleWindow(dpy, XRootWindow(dpy, screen), 0, 0, w, h, 0, 0, 0);
            XMapWindow(dpy, id);
            XSync(dpy, 0);
            Window { dpy, id, screen }
        }
    }

    fn surface(&self) -> origami_surface {
        origami_surface {
            size: std::mem::size_of::<origami_surface>() as u32,
            kind: ORIGAMI_SURFACE_XLIB_WINDOW,
            window: ptr::null_mut(),
            display: self.dpy,
            window_id: self.id as u64,
            screen: self.screen,
        }
    }
}

struct Cube(*mut origami_cube);

impl Cube {
    fn new(window: &Window, w: u32, h: u32, scale: f64) -> Cube {
        let mut cube = ptr::null_mut();
        let surface = window.surface();
        // SAFETY: the window outlives the cube (detached on drop).
        unsafe {
            ok(
                origami_cube_attach(&surface, w, h, scale, &mut cube),
                "cube attach",
            )
        };
        Cube(cube)
    }

    fn style(&self, env: u32, horizon: u32, granite: [u32; 2], spin: bool, reduce: bool) {
        self.scene_style(env, 0, horizon, granite, spin, reduce)
    }

    fn scene_style(
        &self,
        env: u32,
        scene: u32,
        horizon: u32,
        granite: [u32; 2],
        spin: bool,
        reduce: bool,
    ) {
        let s = origami_cube_style {
            size: std::mem::size_of::<origami_cube_style>() as u32,
            env,
            scene,
            horizon_rgb: horizon,
            granite_light_rgb: granite[0],
            granite_dark_rgb: granite[1],
            light_look: u32::from(granite == LIGHT),
            spin: u32::from(spin),
            reduce_motion: u32::from(reduce),
        };
        // SAFETY: valid cube and style.
        unsafe { ok(origami_cube_set_style(self.0, &s), "set_style") };
    }

    /// Present until a frame is drawn (the cap skips changes that come too
    /// soon after a frame); whether the cube asks for more frames.
    fn present(&self) -> (u32, bool) {
        for _ in 0..100 {
            let (drawn, moving) = self.present_once();
            if !(moving && drawn == ORIGAMI_DRAWN_SKIPPED) {
                return (drawn, moving);
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        panic!("the cube never drew");
    }

    fn present_once(&self) -> (u32, bool) {
        let (mut drawn, mut moving) = (9, 9);
        // SAFETY: valid cube and out pointers.
        unsafe {
            ok(
                origami_cube_present(self.0, &mut drawn, &mut moving),
                "present",
            )
        };
        (drawn, moving != 0)
    }

    fn pointer(&self, phase: u32, x: f32, y: f32, ms: u64) {
        // SAFETY: valid cube.
        unsafe { ok(origami_cube_pointer(self.0, phase, x, y, ms), "pointer") };
    }

    fn stats(&self) -> origami_view_counters {
        let mut s = origami_view_counters {
            size: std::mem::size_of::<origami_view_counters>() as u32,
            reserved: 0,
            presents: 0,
            take_ns: 0,
            draw_ns: 0,
            present_ns: 0,
        };
        // SAFETY: valid cube and out pointer.
        unsafe { ok(origami_cube_stats(self.0, &mut s), "stats") };
        s
    }

    /// The cube's pixels as RGB.
    fn shot(&self) -> Shot {
        let (mut w, mut h, mut stride) = (0u32, 0u32, 0u32);
        // SAFETY: a zero-size call only reports the size.
        let status = unsafe {
            origami_cube_readback(self.0, ptr::null_mut(), 0, &mut w, &mut h, &mut stride)
        };
        assert_eq!(status, ORIGAMI_ERR_BUFFER_TOO_SMALL);
        let mut bgra = vec![0u8; (stride * h) as usize];
        // SAFETY: `bgra` is writable for its length.
        unsafe {
            ok(
                origami_cube_readback(
                    self.0,
                    bgra.as_mut_ptr(),
                    bgra.len(),
                    &mut w,
                    &mut h,
                    &mut stride,
                ),
                "cube readback",
            )
        };
        (
            w,
            h,
            bgra.chunks(4).flat_map(|p| [p[2], p[1], p[0]]).collect(),
        )
    }
}

impl Drop for Cube {
    fn drop(&mut self) {
        // SAFETY: made by origami_cube_attach, detached once.
        unsafe { origami_cube_detach(self.0) };
    }
}

fn save(name: &str, shot: &Shot) {
    let Some(dir) = std::env::var_os("ORIGAMI_CUBE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), shot.0, shot.1);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(&shot.2)
        .unwrap();
}

fn scene_text(index: u32, field: u32) -> String {
    let mut buf = [0 as std::ffi::c_char; 256];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { origami_cube_scene_text(index, field, buf.as_mut_ptr(), buf.len()) };
    assert!(n > 0 && n < buf.len());
    // SAFETY: NUL terminated by origami_cube_scene_text.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// Two readbacks of the same picture.
fn same(a: &Shot, b: &Shot) -> bool {
    a.2 == b.2
}

#[test]
#[ignore = "needs a GPU and an X server; see the header"]
fn every_environment_is_drawn_and_the_pointer_and_alt_click_act() {
    let window = Window::new(320, 320);
    let cube = Cube::new(&window, 320, 320, 1.0);
    let mut seen: Vec<Shot> = Vec::new();
    let mut check = |cube: &Cube, name: &str| {
        let (drawn, moving) = cube.present();
        assert!(drawn == ORIGAMI_DRAWN_PRESENTED || drawn == ORIGAMI_DRAWN_OCCLUDED);
        assert!(!moving, "{name}: a still cube is at rest");
        let shot = cube.shot();
        save(name, &shot);
        assert!(
            seen.iter().all(|s| !same(s, &shot)),
            "{name} repeats an earlier picture"
        );
        seen.push(shot);
    };
    cube.style(ORIGAMI_CUBE_ENV_STUDIO, TEAL, TINTED, false, true);
    check(&cube, "start-studio-tinted");
    // Every scene in the manifest, in both appearances; About's pick for each
    // appearance is among them.
    let count = origami_cube_scene_count();
    assert!(count >= 3);
    for (suffix, granite) in [("dark", DARK), ("light", LIGHT)] {
        for scene in 0..count {
            cube.scene_style(
                ORIGAMI_CUBE_ENV_SCENE,
                scene,
                ORIGAMI_CUBE_NO_COLOUR,
                granite,
                false,
                true,
            );
            check(
                &cube,
                &format!(
                    "about-{}-{suffix}",
                    scene_text(scene, ORIGAMI_CUBE_SCENE_ID)
                ),
            );
        }
    }
    let (dark, light) = (origami_cube_about_scene(1), origami_cube_about_scene(0));
    assert_eq!(scene_text(dark, ORIGAMI_CUBE_SCENE_ID), "vancouver-dusk");
    assert_eq!(scene_text(light, ORIGAMI_CUBE_SCENE_ID), "howe-sound");
    assert!(scene_text(light, ORIGAMI_CUBE_SCENE_CREDIT).contains("CC BY 2.0"));
    let found = CString::new("garibaldi-lake").unwrap();
    // SAFETY: a NUL terminated string.
    assert_eq!(unsafe { origami_cube_scene_find(found.as_ptr()) }, 1);

    // Reduce motion: no spin, one frame, nothing more asked for. A drag turns
    // the cube and it stops dead on release.
    cube.scene_style(
        ORIGAMI_CUBE_ENV_SCENE,
        origami_cube_about_scene(1),
        ORIGAMI_CUBE_NO_COLOUR,
        DARK,
        false,
        true,
    );
    cube.present();
    let before = cube.shot();
    cube.pointer(ORIGAMI_CUBE_POINTER_PRESS, 100.0, 100.0, 0);
    cube.pointer(ORIGAMI_CUBE_POINTER_MOVE, 160.0, 120.0, 16);
    cube.pointer(ORIGAMI_CUBE_POINTER_RELEASE, 160.0, 120.0, 20);
    let (_, moving) = cube.present();
    assert!(!moving, "reduce motion stops dead on release");
    let turned = cube.shot();
    save("about-dragged", &turned);
    assert!(!same(&before, &turned));

    // A double click eases back to the mark pose.
    cube.pointer(ORIGAMI_CUBE_POINTER_DOUBLE_CLICK, 0.0, 0.0, 1000);
    for _ in 0..70 {
        std::thread::sleep(Duration::from_millis(16));
        cube.present();
    }
    assert!(same(&cube.shot(), &before), "back in the mark pose");

    // Alt-click moves to the next scene.
    let first = cube.shot();
    // SAFETY: valid cube.
    unsafe { ok(origami_cube_alt_click(cube.0), "alt click") };
    cube.present();
    assert!(!same(&first, &cube.shot()));
}

#[test]
#[ignore = "needs a GPU and an X server; see the header"]
fn the_cube_reflects_the_machines_screen() {
    let lab = Lab::new(None);
    let machine = lab.c_machine();
    let window = Window::new(64, 48);
    let screen_surface = window.surface();
    let mut view = ptr::null_mut();
    // SAFETY: the window outlives the view (detached below).
    unsafe {
        ok(
            origami_view_attach(machine, 0, &screen_surface, 64, 48, 1.0, &mut view),
            "attach",
        )
    };
    let cube_window = Window::new(256, 256);
    let cube = Cube::new(&cube_window, 256, 256, 1.0);
    cube.style(
        ORIGAMI_CUBE_ENV_MACHINE,
        ORIGAMI_CUBE_NO_COLOUR,
        DARK,
        false,
        true,
    );
    // No picture has been copied yet: the studio, not black.
    cube.present();
    let studio = cube.shot();
    // SAFETY: valid cube and machine.
    unsafe { ok(origami_cube_set_machine(cube.0, machine, 0), "set_machine") };
    for _ in 0..3 {
        lab.advance();
        // SAFETY: valid view.
        unsafe { ok(origami_view_present(view, ptr::null_mut()), "present") };
    }
    cube.present();
    let reflected = cube.shot();
    save("about-machine-screen", &reflected);
    assert!(!same(&studio, &reflected), "the screen is in the chrome");
    // A NULL machine stops the copying and leaves the last picture.
    // SAFETY: valid cube.
    unsafe { ok(origami_cube_set_machine(cube.0, ptr::null(), 0), "clear") };
    // SAFETY: made by origami_view_attach, detached once.
    unsafe { origami_view_detach(view) };
}

#[test]
#[ignore = "needs a GPU and an X server; see the header"]
fn cpu_while_turning_at_rest_and_hidden() {
    let secs: f64 = std::env::var("ORIGAMI_CUBE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    // The start screen's cube: 96 points on a Retina display.
    let window = Window::new(192, 192);
    let cube = Cube::new(&window, 192, 192, 2.0);
    cube.style(ORIGAMI_CUBE_ENV_STUDIO, TEAL, TINTED, true, false);
    // The host's display tick: a vsync callback, 60 a second.
    let tick = Duration::from_micros(16_667);
    cube.present();

    let (cpu0, wall0, frames0, ticks0) =
        (process_cpu(), Instant::now(), cube.stats(), thread_ticks());
    let mut next = Instant::now();
    while wall0.elapsed().as_secs_f64() < secs {
        let (_, moving) = cube.present_once();
        assert!(moving);
        next += tick;
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    let (cpu, wall, frames) = (
        process_cpu() - cpu0,
        wall0.elapsed().as_secs_f64(),
        cube.stats(),
    );
    let n = (frames.presents - frames0.presents) as f64;
    eprintln!(
        "MEASURE turning: {n} frames in {wall:.1} s ({:.1}/s), process CPU {:.2}% of one core, \
         draw {:.0} us, present {:.0} us per frame (wall)",
        n / wall,
        cpu / wall * 100.0,
        (frames.draw_ns - frames0.draw_ns) as f64 / n / 1e3,
        (frames.present_ns - frames0.present_ns) as f64 / n / 1e3,
    );
    for (name, ticks) in thread_ticks() {
        let spent = ticks - ticks0.get(&name).copied().unwrap_or(0);
        if spent > 0 {
            eprintln!("MEASURE thread {name}: {spent} ticks of 10 ms");
        }
    }
    save("start-turning", &cube.shot());
    assert!(
        n / wall < 40.0,
        "only the calm spin turns it: about 30 a second"
    );

    // A fling is drawn at the full 60 a second until friction brings it back.
    let (cpu0, wall0, frames0) = (process_cpu(), Instant::now(), cube.stats());
    cube.pointer(ORIGAMI_CUBE_POINTER_PRESS, 20.0, 40.0, 0);
    cube.pointer(ORIGAMI_CUBE_POINTER_MOVE, 40.0, 40.0, 16);
    cube.pointer(ORIGAMI_CUBE_POINTER_RELEASE, 40.0, 40.0, 20);
    let mut next = Instant::now();
    while wall0.elapsed() < Duration::from_millis(1200) {
        cube.present_once();
        next += tick;
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    let n = (cube.stats().presents - frames0.presents) as f64;
    eprintln!(
        "MEASURE flung: {n} frames in {:.1} s, process CPU {:.2}% of one core",
        wall0.elapsed().as_secs_f64(),
        (process_cpu() - cpu0) / wall0.elapsed().as_secs_f64() * 100.0
    );

    // At rest: one frame, then the host stops calling.
    cube.style(ORIGAMI_CUBE_ENV_STUDIO, TEAL, TINTED, false, true);
    let (_, moving) = cube.present();
    assert!(!moving);
    let rest0 = cube.stats().presents;
    let (cpu0, wall0) = (process_cpu(), Instant::now());
    std::thread::sleep(Duration::from_secs(3));
    eprintln!(
        "MEASURE at rest: {} presents and process CPU {:.3}% over {:.1} s with the host idle",
        cube.stats().presents - rest0,
        (process_cpu() - cpu0) / wall0.elapsed().as_secs_f64() * 100.0,
        wall0.elapsed().as_secs_f64()
    );
    // Asked anyway (a stray refresh): nothing is drawn.
    let (drawn, moving) = cube.present();
    assert_eq!((drawn, moving), (ORIGAMI_DRAWN_SKIPPED, false));
    assert_eq!(cube.stats().presents, rest0);

    // Hidden while it would spin.
    cube.style(ORIGAMI_CUBE_ENV_STUDIO, TEAL, TINTED, true, false);
    // SAFETY: valid cube.
    unsafe { ok(origami_cube_set_visible(cube.0, 0), "hide") };
    let hidden0 = cube.stats().presents;
    for _ in 0..30 {
        let (drawn, moving) = cube.present();
        assert_eq!((drawn, moving), (ORIGAMI_DRAWN_SKIPPED, false));
    }
    assert_eq!(cube.stats().presents, hidden0);
}

/// A cube with no window hands over its frames; needs a GPU but no X server.
#[test]
fn an_offscreen_cube_draws_and_hands_over_its_frames() {
    let mut cube = ptr::null_mut();
    // SAFETY: a valid out pointer.
    let status = unsafe { origami_cube_attach_offscreen(96, 64, 1.0, &mut cube) };
    if status == ORIGAMI_ERR_NO_GPU {
        eprintln!("skipping: no GPU");
        return;
    }
    ok(status, "attach offscreen");
    let cube = Cube(cube);
    let mut size = (0u32, 0u32, 0u32);
    let mut buf = vec![0u8; 96 * 64 * 4];
    // SAFETY: valid cube and buffer.
    let before = unsafe {
        origami_cube_readback(
            cube.0,
            buf.as_mut_ptr(),
            buf.len(),
            &mut size.0,
            &mut size.1,
            &mut size.2,
        )
    };
    assert_eq!(before, ORIGAMI_ERR_NOT_FOUND);
    cube.style(
        ORIGAMI_CUBE_ENV_STUDIO,
        ORIGAMI_CUBE_NO_COLOUR,
        DARK,
        true,
        false,
    );
    let (drawn, moving) = cube.present();
    assert_eq!((drawn, moving), (ORIGAMI_DRAWN_PRESENTED, true));
    let first = cube.shot();
    assert_eq!((first.0, first.1), (96, 64));
    // The same frame again: reading back does not draw.
    assert!(same(&first, &cube.shot()));
    std::thread::sleep(Duration::from_millis(100));
    cube.present();
    assert!(!same(&first, &cube.shot()), "the calm spin moved it");
    assert_eq!(cube.stats().presents, 2);
}

/// CPU of an offscreen cube turning, as the GTK start screen draws it: a frame
/// every 33 ms, copied out each time. Needs a GPU, not an X server.
#[test]
#[ignore = "measures; needs a GPU"]
fn offscreen_cpu_while_turning() {
    let secs: f64 = std::env::var("ORIGAMI_CUBE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    for (name, px, scale) in [("108 pt at 2x", 216, 2.0), ("108 pt at 1x", 108, 1.0)] {
        let mut cube = ptr::null_mut();
        // SAFETY: a valid out pointer.
        ok(
            unsafe { origami_cube_attach_offscreen(px, px, scale, &mut cube) },
            "attach offscreen",
        );
        let cube = Cube(cube);
        cube.style(ORIGAMI_CUBE_ENV_STUDIO, TEAL, TINTED, true, false);
        cube.present();
        let (cpu0, wall0, n0) = (process_cpu(), Instant::now(), cube.stats().presents);
        let mut next = Instant::now();
        while wall0.elapsed().as_secs_f64() < secs {
            if cube.present_once().0 == ORIGAMI_DRAWN_PRESENTED {
                cube.shot();
            }
            next += Duration::from_millis(16);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        let (cpu, wall) = (process_cpu() - cpu0, wall0.elapsed().as_secs_f64());
        let n = (cube.stats().presents - n0) as f64;
        eprintln!(
            "MEASURE offscreen {name}: {n} frames in {wall:.1} s ({:.1}/s), process CPU {:.2}% of one core",
            n / wall,
            cpu / wall * 100.0
        );
    }
}
