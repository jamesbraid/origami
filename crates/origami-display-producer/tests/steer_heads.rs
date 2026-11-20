//! Follow mode reads the steering block of the head the viewer shows, not
//! head 0's, on a machine with several heads.

use origami_display::v4::sys::{UnixListener, UnixStream};
use origami_display::v4::viewer::Viewer;
use origami_display::v4::wire;
use origami_display_producer::reference::PullHost;
use origami_display_producer::{Config, HeadSpec, Producer};
use origami_engine::InputEvent;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_shown_heads_block_is_followed() {
    let dir = std::env::temp_dir().join(format!("steer-heads-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("d.sock");
    let cfg = Config {
        steering: true,
        ..Default::default()
    };
    let p = Producer::start(
        cfg,
        UnixListener::bind(&path).unwrap(),
        Box::new(PullHost::new()),
        Producer::waker().unwrap(),
    )
    .unwrap();
    let spec = HeadSpec {
        pointer_mode: wire::POINTER_STEERED,
        ..Default::default()
    };
    p.with(|e| {
        for _ in 0..3 {
            e.add_head(spec.clone()).unwrap();
        }
    });
    let h = p.handle();
    h.steer(0, 0, 7, 7);
    h.steer(1, 1, 111, 222);
    h.steer(2, 1, 333, 444);

    let viewer = Arc::new(Viewer::new());
    let stream = UnixStream::connect(&path).unwrap();
    let thread = {
        let v = viewer.clone();
        std::thread::spawn(move || {
            let _ = v.run(stream, &|| {});
        })
    };
    // Each call re-aims the viewer at the first head with a picture.
    let touch = || {
        let _ = viewer.send_input(&[InputEvent::PointerAt {
            head: 0,
            x: 1,
            y: 1,
        }]);
        viewer.followed_cursor().map(|(_, x, y)| (x, y))
    };
    until("three heads", || viewer.heads().len() == 3);
    assert_eq!(touch(), None);
    p.with(|e| e.remove_head(0));
    until("head 1 followed", || touch() == Some((111, 222)));
    p.with(|e| e.remove_head(1));
    until("head 2 followed", || touch() == Some((333, 444)));

    viewer.close();
    thread.join().unwrap();
    p.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
