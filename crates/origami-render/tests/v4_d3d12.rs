//! The version 4 viewer's Direct3D 12 heap class against the reference producer;
//! ends early on an adapter that cannot open existing heaps.
#![cfg(windows)]

use origami_display::v4::sys::{UnixListener, UnixStream};
use origami_display::v4::viewer::Viewer;
use origami_display::Taken;
use origami_display_producer::reference;
use origami_render::{Gpu, GpuImporter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn a_slot_section_is_drawn_from_in_place_when_the_adapter_allows_it() {
    let gpu = Gpu::new().expect("a Direct3D 12 adapter");
    eprintln!("adapter import: {}", gpu.import.describe());
    let Some(importer) = GpuImporter::new(&gpu) else {
        eprintln!("D3D12 HEAP: unavailable on this adapter; the copy class applies");
        return;
    };
    let dir = std::env::temp_dir().join(format!("v4d3d12-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("d.sock");
    let producer =
        reference::start(UnixListener::bind(&path).unwrap(), 1, 256, 128, false).unwrap();
    let viewer = Arc::new(Viewer::new());
    viewer.set_importer(importer);
    let wakes = Arc::new(AtomicU64::new(0));
    let stream = UnixStream::connect(&path).unwrap();
    let thread = {
        let (viewer, wakes) = (viewer.clone(), wakes.clone());
        std::thread::spawn(move || {
            let wake = || {
                wakes.fetch_add(1, Ordering::SeqCst);
            };
            let _ = viewer.run(stream, &wake);
        })
    };
    let t = Instant::now();
    while wakes.load(Ordering::SeqCst) == 0 {
        assert!(t.elapsed() < Duration::from_secs(10), "no first frame");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut px = Vec::new();
    let taken = viewer.take(0, &mut px);
    let backend = viewer.backend();
    eprintln!(
        "D3D12 HEAP: backend {backend:?}, stats {:?}",
        viewer.stats()
    );
    viewer.close();
    thread.join().unwrap();
    producer.stop();
    let _ = std::fs::remove_dir_all(dir);
    match taken {
        Taken::Imported(_) => {}
        other => panic!(
            "heap import expected, got {}",
            matches!(other, Taken::Frame(_))
        ),
    }
}
