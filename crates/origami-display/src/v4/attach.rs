//! A machine's display attached in this process over a version 4 socket: the
//! frame source and input an engine hands its front end when the producer
//! is another process.

use super::session::SessionError;
use super::viewer::Viewer;
use crate::{Importer, Taken};
use origami_engine::{
    CursorInfo, Frame, FrameSource, FrameStats, HeadInfo, InputEvent, MachineId, Result, Wake,
};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Interrupt = Box<dyn Fn() + Send + Sync>;

pub struct Attached {
    viewer: Arc<Viewer>,
    id: MachineId,
    wake: Wake,
    stop: Arc<AtomicBool>,
    interrupt: Arc<Mutex<Option<Interrupt>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Attached {
    /// Serve the socket at `socket` until dropped, reconnecting while
    /// `alive` says the producer exists. `wake` is called when a head has
    /// something new and the view is shown.
    pub fn connect(
        socket: PathBuf,
        importer: Option<Arc<dyn Importer>>,
        id: MachineId,
        wake: Wake,
        alive: impl Fn() -> bool + Send + 'static,
    ) -> std::io::Result<Arc<Attached>> {
        let viewer = Arc::new(Viewer::new());
        if let Some(importer) = importer {
            viewer.set_importer(importer);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let interrupt: Arc<Mutex<Option<Interrupt>>> = Arc::default();
        let thread = {
            let (viewer, wake, stop, interrupt) = (
                viewer.clone(),
                wake.clone(),
                stop.clone(),
                interrupt.clone(),
            );
            std::thread::Builder::new()
                .name("v4-display".into())
                .spawn(move || serve(&socket, &viewer, id, &wake, &stop, &interrupt, alive))?
        };
        Ok(Arc::new(Attached {
            viewer,
            id,
            wake,
            stop,
            interrupt,
            thread: Mutex::new(Some(thread)),
        }))
    }

    /// Typed input as one report.
    pub fn send_input(&self, events: &[InputEvent]) -> Result<()> {
        self.viewer.send_input(events)
    }
}

fn serve(
    socket: &PathBuf,
    viewer: &Viewer,
    id: MachineId,
    wake: &Wake,
    stop: &AtomicBool,
    interrupt: &Mutex<Option<Interrupt>>,
    alive: impl Fn() -> bool,
) {
    let wake = || {
        if viewer.visible() {
            wake(id);
        }
    };
    while !stop.load(Ordering::Acquire) {
        let mut retry = Duration::from_millis(200);
        let connected = UnixStream::connect(socket).and_then(|stream| {
            // Without a second handle nothing could end a blocked read.
            let end = stream.try_clone()?;
            Ok((stream, end))
        });
        match connected {
            Ok((stream, end)) => {
                *lock(interrupt) = Some(Box::new(move || drop(end.shutdown(Shutdown::Both))));
                // A drop that came before the interrupt was stored would wait forever.
                if stop.load(Ordering::Acquire) {
                    return;
                }
                if let Err(e) = viewer.run(stream, &wake) {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    eprintln!("origami: display: {e}");
                    // A producer that breaks the protocol will do it again.
                    if matches!(e, SessionError::Protocol(_) | SessionError::Producer(_)) {
                        return;
                    }
                }
                retry = Duration::from_secs(1);
            }
            Err(e) => viewer.connect_failed(e.to_string()),
        }
        if stop.load(Ordering::Acquire) || !alive() {
            return;
        }
        std::thread::sleep(retry);
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.viewer.close();
        if let Some(interrupt) = lock(&self.interrupt).as_ref() {
            interrupt();
        }
        if let Some(thread) = lock(&self.thread).take() {
            let _ = thread.join();
        }
    }
}

impl FrameSource for Attached {
    fn heads(&self) -> Vec<HeadInfo> {
        self.viewer.heads()
    }

    fn take_frame(&self, head: usize, pixels: &mut Vec<u8>) -> Option<Frame> {
        match self.viewer.take(head, pixels) {
            Taken::Frame(update) => Some(Frame::Rows(update)),
            Taken::Imported(imported) => Some(Frame::Imported(imported)),
            _ => None,
        }
    }

    fn cursor(&self, head: usize, known_gen: u32) -> Option<CursorInfo> {
        self.viewer.cursor(head, known_gen)
    }

    fn set_visible(&self, visible: bool) {
        if self.viewer.set_visible(visible) {
            (self.wake)(self.id);
        }
    }

    fn stats(&self) -> Option<FrameStats> {
        let s = self.viewer.stats();
        Some(FrameStats {
            frames: s.frames,
            lines: s.lines,
            imported_lines: s.imported_lines,
            copied_bytes: s.copied_bytes,
        })
    }
}
