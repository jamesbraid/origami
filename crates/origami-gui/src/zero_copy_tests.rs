//! A real QEMU's scanout memory taken into this process's GPU and drawn, then
//! compared with what QEMU says the screen shows (a QMP `screendump`).
//!
//! Ignored by default: it starts a machine. It needs a Vulkan device (any;
//! `VK_DRIVER_FILES` picks one), and these:
//!
//! - ORIGAMI_RUNTIME_DIR: a directory holding the QEMU the product runs, built
//!   from a QEMU with protocol version 3 (`display-shm`, `display-shm-gpu`)
//! - ORIGAMI_BIN: the `origami` executable, which creates and starts the machine
//! - ORIGAMI_GUI_TEST_DIR: an empty or new directory for the machine
//!
//! `ORIGAMI_GUI_ZERO_COPY=0` runs the same test on the copying path, to
//! compare the counters. Run it with
//! `cargo test -p origami-gui zero_copy -- --ignored --nocapture`.

use crate::core::{Frame, InputEvent, Key};
use crate::qemu::RunnerEngine;
use origami_core::facade::{Config, Core, Machine, StopKind};
use origami_render::{Compose, Gpu, GpuImporter, HeadTexture};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} is not set; see the header of zero_copy_tests.rs"))
}

/// The head's pixels as RGB, as the GPU holds them.
fn read_back(gpu: &Gpu, t: &wgpu::Texture) -> Vec<u8> {
    let (w, h) = (t.width(), t.height());
    let padded = (w * 4).next_multiple_of(256);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(padded * h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        t.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
    let data = buf.slice(..).get_mapped_range().expect("mapped").to_vec();
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for row in data.chunks(padded as usize).take(h as usize) {
        for px in row[..(w * 4) as usize].chunks(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]); // bgra
        }
    }
    rgb
}

/// QEMU's own idea of the screen: a QMP `screendump`, as PPM.
fn screendump(qmp_port: u16, to: &Path) -> (u32, u32, Vec<u8>) {
    let stream = TcpStream::connect(("127.0.0.1", qmp_port)).expect("QMP");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut out = stream;
    let mut line = String::new();
    reader.read_line(&mut line).unwrap(); // greeting
    let mut send = |cmd: &str| {
        writeln!(out, "{cmd}").unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.contains("\"return\"") {
                return;
            }
            assert!(!line.contains("\"error\""), "{cmd}: {line}");
        }
    };
    send(r#"{"execute":"qmp_capabilities"}"#);
    send(&format!(
        r#"{{"execute":"screendump","arguments":{{"filename":"{}","format":"ppm"}}}}"#,
        to.display()
    ));
    let mut ppm = Vec::new();
    std::fs::File::open(to)
        .unwrap()
        .read_to_end(&mut ppm)
        .unwrap();
    // "P6\nW H\n255\n" then RGB.
    let mut at = 0;
    let mut field = || {
        while ppm[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !ppm[at].is_ascii_whitespace() {
            at += 1;
        }
        String::from_utf8_lossy(&ppm[start..at]).into_owned()
    };
    assert_eq!(field(), "P6");
    let (w, h): (u32, u32) = (field().parse().unwrap(), field().parse().unwrap());
    assert_eq!(field(), "255");
    at += 1;
    (w, h, ppm[at..].to_vec())
}

struct Run {
    gpu: Gpu,
    engine: Arc<RunnerEngine>,
    compose: Compose,
    machine: Machine,
    head: Option<HeadTexture>,
    pixels: Vec<u8>,
    imported_frames: u64,
    copied_frames: u64,
    last_frame: Instant,
    qmp_port: u16,
    dir: PathBuf,
}

impl Run {
    /// Take what the engine has and draw it.
    fn pump(&mut self) {
        let hs = self.machine.heads();
        let Some(info) = hs.first().filter(|h| h.active) else {
            return;
        };
        if self.head.as_ref().is_some_and(|h| h.seq >= info.seq) {
            return;
        }
        match self.machine.take_frame(0, &mut self.pixels).unwrap() {
            Some(Frame::Imported(f)) => {
                self.imported_frames += 1;
                self.head = Some(HeadTexture::apply_imported(
                    &self.gpu,
                    &self.compose,
                    self.head.take(),
                    &f,
                ));
                self.last_frame = Instant::now();
            }
            Some(Frame::Rows(u)) => {
                self.copied_frames += 1;
                self.head = Some(HeadTexture::apply(
                    &self.gpu,
                    self.head.take(),
                    &u,
                    &self.pixels,
                ));
                self.last_frame = Instant::now();
            }
            None => {}
        }
    }

    /// Pump until the picture has been still for `quiet`.
    fn settle(&mut self, quiet: Duration, limit: Duration) {
        let start = Instant::now();
        self.last_frame = Instant::now();
        loop {
            self.pump();
            if self.head.is_some() && self.last_frame.elapsed() > quiet {
                return;
            }
            assert!(start.elapsed() < limit, "the picture never settled");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The GPU's picture against QEMU's screendump: the number of pixels that
    /// differ, and the number of distinct colours in the picture.
    fn compare(&mut self, name: &str) -> (usize, usize) {
        let (w, h, want) = screendump(self.qmp_port, &self.dir.join(format!("{name}.ppm")));
        let head = self.head.as_ref().expect("a picture");
        assert_eq!((head.width, head.height), (w, h), "the sizes agree");
        let got = read_back(&self.gpu, &head.texture);
        let differing = got
            .chunks(3)
            .zip(want.chunks(3))
            .filter(|(a, b)| a != b)
            .count();
        let colours: std::collections::HashSet<&[u8]> = want.chunks(3).collect();
        (differing, colours.len())
    }
}

#[test]
#[ignore = "starts a machine; see the header"]
fn zero_copy_regions_draw_what_qemu_says_the_screen_shows() {
    // One directory for each run, so runs of the two paths can overlap.
    let work = var("ORIGAMI_GUI_TEST_DIR").join(format!(
        "zero-copy-{}",
        std::env::var("ORIGAMI_GUI_TEST_NAME").unwrap_or_else(|_| std::process::id().to_string())
    ));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let gpu = Gpu::new().expect("a GPU");
    eprintln!("GPU {}", gpu.describe());
    eprintln!("zero copy: {}", gpu.import.describe());
    let want_import = std::env::var("ORIGAMI_GUI_ZERO_COPY").map_or(true, |v| v != "0");
    assert_eq!(
        gpu.import.kind.is_some(),
        want_import,
        "{}",
        gpu.import.describe()
    );

    let origami = var("ORIGAMI_BIN");
    let machine = work.join("octane2");
    let out = Command::new(&origami)
        .arg("create")
        .arg(&machine)
        .args(["--preset", "octane2-vpro"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let runtime = origami_qemu::Runtime::locate().expect("a runtime");
    let engine = Arc::new(RunnerEngine::new(runtime, origami));
    if let Some(importer) = GpuImporter::new(&gpu) {
        engine.set_importer(importer);
    }
    let core = Core::new(Config::new(&work), vec![engine.clone()], None).unwrap();
    let id = core
        .start_machine(&machine.to_string_lossy())
        .expect("the machine starts");
    struct Stop(Machine);
    impl Drop for Stop {
        fn drop(&mut self) {
            if let Ok(op) = self.0.stop(StopKind::Clean) {
                op.wait(None);
            }
        }
    }
    let _stop = Stop(id.clone());
    let qmp_port = origami_machine::control::read(&machine.canonicalize().unwrap())
        .expect("a running machine")
        .qmp_port;
    let mut run = Run {
        compose: Compose::new(&gpu),
        gpu,
        engine,
        machine: id,
        head: None,
        pixels: Vec::new(),
        imported_frames: 0,
        copied_frames: 0,
        last_frame: Instant::now(),
        qmp_port,
        dir: work.clone(),
    };

    // The PROM runs its diagnostics, which fail, and stops on a dialog. Wait
    // for a picture with real content that has then been still for a while.
    let start = Instant::now();
    loop {
        run.settle(Duration::from_secs(4), Duration::from_secs(900));
        let (differing, colours) = run.compare("prom");
        eprintln!(
            "after {:?}: {differing} pixels differ, {colours} colours",
            start.elapsed()
        );
        // The PROM's dialog on its gradient is well over 100 colours; the
        // diagnostics before it are a few.
        if colours > 100 {
            assert_eq!(differing, 0, "the GPU's picture is not QEMU's");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(900),
            "the PROM never drew its dialog"
        );
    }
    let first = read_back(&run.gpu, &run.head.as_ref().unwrap().texture);
    let backend = run
        .engine
        .display_backend(&machine.canonicalize().unwrap())
        .unwrap_or_default();
    eprintln!("backend: {backend}");

    // A change: a key dismisses the dialog and the PROM draws its menu.
    for down in [true, false] {
        run.machine
            .send_input(&[InputEvent::Key {
                key: Key::Space,
                down,
            }])
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    let seq = run.head.as_ref().unwrap().seq;
    let start = Instant::now();
    while run.head.as_ref().unwrap().seq == seq {
        run.pump();
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "the guest did not repaint"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    run.settle(Duration::from_secs(4), Duration::from_secs(120));
    let (differing, colours) = run.compare("after-key");
    let second = read_back(&run.gpu, &run.head.as_ref().unwrap().texture);
    let changed = first
        .chunks(3)
        .zip(second.chunks(3))
        .filter(|(a, b)| a != b)
        .count();
    eprintln!("after a key: {differing} pixels differ from QEMU's, {colours} colours, {changed} pixels changed");
    assert_eq!(
        differing, 0,
        "the GPU's picture is not QEMU's after a change"
    );
    assert!(
        changed > 10_000,
        "the picture hardly changed ({changed} pixels)"
    );

    let stats = run.engine.stats(&machine.canonicalize().unwrap()).unwrap();
    let head = run.head.as_ref().unwrap();
    eprintln!(
        "frames: {} imported, {} copied; engine copied {} bytes, imported {} lines; \
         texture: {} rows imported, {} rows ({} bytes) uploaded from the CPU",
        run.imported_frames,
        run.copied_frames,
        stats.copied_bytes,
        stats.imported_lines,
        head.rows_imported,
        head.rows_uploaded,
        head.bytes_uploaded
    );
    if want_import {
        assert!(backend.contains("import"), "{backend}");
        assert!(run.imported_frames > 0 && run.copied_frames == 0);
        assert_eq!(stats.copied_bytes, 0, "a pixel was copied by the CPU");
        assert_eq!(head.bytes_uploaded, 0);
        assert!(head.rows_imported > 0 && stats.imported_lines > 0);
    } else {
        assert!(run.copied_frames > 0 && run.imported_frames == 0);
        assert!(stats.copied_bytes > 0 && head.bytes_uploaded > 0);
    }
    assert_eq!(run.gpu.errors.count(), 0);
}
