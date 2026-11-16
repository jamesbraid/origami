//! The guest's cursor reaches the window: a fake machine's 8 by 8 cursor is
//! drawn into an X11 window by the C view functions. Ignored: needs a Vulkan
//! device, an X server (`DISPLAY`) and ImageMagick's `import`.
#![cfg(all(unix, feature = "fake", feature = "render"))]
#![allow(clippy::unnecessary_cast)]

mod common;

use common::*;
use origami_ffi::*;
use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void, CString};
use std::process::Command;
use std::ptr;

#[link(name = "X11")]
extern "C" {
    fn XOpenDisplay(name: *const c_char) -> *mut c_void;
    fn XDefaultScreen(d: *mut c_void) -> c_int;
    fn XRootWindow(d: *mut c_void, screen: c_int) -> c_ulong;
    fn XCreateSimpleWindow(
        d: *mut c_void,
        parent: c_ulong,
        x: c_int,
        y: c_int,
        w: c_uint,
        h: c_uint,
        border_width: c_uint,
        border: c_ulong,
        background: c_ulong,
    ) -> c_ulong;
    fn XMapWindow(d: *mut c_void, w: c_ulong) -> c_int;
    fn XSync(d: *mut c_void, discard: c_int) -> c_int;
}

/// The window's pixels as RGB, 64 by 48 (the fake picture's size).
fn shot(window: c_ulong, to: &std::path::Path) -> Vec<u8> {
    let st = Command::new("import")
        .args(["-window", &window.to_string(), "-depth", "8"])
        .arg(format!("ppm:{}", to.display()))
        .status()
        .expect("ImageMagick import");
    assert!(st.success());
    let bytes = std::fs::read(to).unwrap();
    let header = b"P6\n64 48\n255\n";
    assert!(bytes.starts_with(header), "unexpected PPM header");
    bytes[header.len()..].to_vec()
}

/// The window once its pixels stop changing: a present reaches the X server
/// a moment after the call returns.
fn settled(window: c_ulong, to: &std::path::Path) -> Vec<u8> {
    let mut last = shot(window, to);
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let now = shot(window, to);
        if now == last {
            return now;
        }
        last = now;
    }
    panic!("the window never settled");
}

#[test]
#[ignore = "needs a GPU and an X server; see the header"]
fn the_guests_cursor_is_drawn_over_the_picture() {
    let lab = Lab::new(None);
    let machine = lab.c_machine();
    let key = CString::new(lab.key.as_str()).unwrap();
    // SAFETY: Xlib calls on a display we open and keep.
    let (dpy, window, screen) = unsafe {
        let dpy = XOpenDisplay(ptr::null());
        assert!(!dpy.is_null(), "no X display; set DISPLAY");
        let screen = XDefaultScreen(dpy);
        let window = XCreateSimpleWindow(dpy, XRootWindow(dpy, screen), 0, 0, 64, 48, 0, 0, 0);
        XMapWindow(dpy, window);
        XSync(dpy, 0);
        (dpy, window, screen)
    };
    let surface = origami_surface {
        size: std::mem::size_of::<origami_surface>() as u32,
        kind: ORIGAMI_SURFACE_XLIB_WINDOW,
        window: ptr::null_mut(),
        display: dpy,
        window_id: window as u64, // c_ulong is wider elsewhere
        screen,
    };
    let mut view = ptr::null_mut();
    let present = |view| {
        let mut drawn = 9;
        // SAFETY: valid view and out pointer.
        assert_eq!(
            unsafe { origami_view_present(view, &mut drawn) },
            ORIGAMI_OK
        );
        drawn
    };
    // SAFETY: the window outlives the view, detached below.
    assert_eq!(
        unsafe { origami_view_attach(machine, 0, &surface, 64, 48, 1.0, &mut view) },
        ORIGAMI_OK,
        "{}",
        last_error()
    );
    let dir = tempfile::tempdir().unwrap();
    // NVIDIA's presentation to Xvfb shows the previous image until the next
    // present, so each shot below follows two draws.
    assert_eq!(present(view), ORIGAMI_DRAWN_PRESENTED);
    assert_eq!(present(view), ORIGAMI_DRAWN_PRESENTED);
    let plain = settled(window, &dir.path().join("plain.ppm"));

    // SAFETY: valid arguments.
    assert_eq!(
        unsafe { origami_fake_show_cursor(lab.c_core, key.as_ptr(), 20, 10) },
        ORIGAMI_OK
    );
    assert_eq!(present(view), ORIGAMI_DRAWN_PRESENTED);
    assert_eq!(present(view), ORIGAMI_DRAWN_PRESENTED);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let with = settled(window, &dir.path().join("with.ppm"));
    let differ: Vec<usize> = plain
        .chunks(3)
        .zip(with.chunks(3))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    // The cursor covers x 20..28, y 10..18, and is white over a picture that
    // is nowhere white.
    assert_eq!(differ.len(), 64, "the cursor's 8 by 8 pixels");
    for i in differ {
        let (x, y) = (i % 64, i / 64);
        assert!((20..28).contains(&x) && (10..18).contains(&y), "{x},{y}");
        assert_eq!(&with[i * 3..i * 3 + 3], &[255, 255, 255]);
    }
    // SAFETY: detached once, before the window goes.
    unsafe { origami_view_detach(view) };
    unsafe { origami_machine_release(machine) };
}
