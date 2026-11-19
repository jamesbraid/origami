//! The iris host publishes the guest's cursor register for the viewer to steer by.

use origami_display::v4::sys::{UnixListener, UnixStream};
use origami_display::v4::viewer::Viewer;
use origami_display_producer::{Config, Handle, HeadSpec, Host, InputEvent, Marks, Producer};
use origami_iris::display::follow_cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct NoHost;

impl Host for NoHost {
    fn attach(&mut self, _: u32, _: Arc<Marks>) {}
    fn request_frame(&mut self, _: u32) {}
    fn inject(&mut self, _: &InputEvent) {}
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let t = Instant::now();
    while !ok() {
        assert!(t.elapsed() < Duration::from_secs(5), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_cursor_register_reaches_the_viewer_as_the_guests_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("display.sock");
    let producer = Producer::start(
        Config {
            steering: true,
            ..Default::default()
        },
        UnixListener::bind(&path).unwrap(),
        Box::new(NoHost),
        Producer::waker().unwrap(),
    )
    .unwrap();
    producer.with(|e| {
        e.add_head(HeadSpec {
            width: 64,
            height: 32,
            ..Default::default()
        })
        .unwrap()
    });
    let register: Arc<Mutex<Option<(i32, i32)>>> = Default::default();
    let reg = register.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    let handle: Handle = producer.handle();
    follow_cursor(
        handle,
        0,
        || true,
        move || {
            counted.fetch_add(1, Ordering::SeqCst);
            *reg.lock().unwrap()
        },
    );

    // Nothing is read while no viewer steers.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(reads.load(Ordering::SeqCst), 0);

    let viewer = Arc::new(Viewer::new());
    let v = viewer.clone();
    let stream = UnixStream::connect(&path).unwrap();
    std::thread::spawn(move || v.run(stream, &|| {}));

    wait_for("the cursor was never read for a steering viewer", || {
        reads.load(Ordering::SeqCst) > 0
    });
    // Cursor off: the viewer has nothing to follow.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(viewer.followed_cursor(), None);

    *register.lock().unwrap() = Some((-5, 20));
    wait_for("no cursor after the register was set", || {
        viewer
            .followed_cursor()
            .is_some_and(|(_, x, y)| (x, y) == (-5, 20))
    });
    let (seq, ..) = viewer.followed_cursor().unwrap();

    *register.lock().unwrap() = Some((30, 7));
    wait_for("the cursor did not move with the register", || {
        viewer
            .followed_cursor()
            .is_some_and(|(s, x, y)| s != seq && (x, y) == (30, 7))
    });

    *register.lock().unwrap() = None;
    wait_for("the cursor stayed after it was turned off", || {
        viewer.followed_cursor().is_none()
    });
}
