//! Getting the machine's picture onto the screen.
//!
//! Imported path: the engine names rows of memory the GPU already holds; we
//! copy only the lines that changed into a ring of dma-buf images on the GPU
//! and hand the newest to GTK as a `GdkDmabufTexture` inside a
//! `GtkGraphicsOffload`. No CPU touches a pixel. Fallback path (no Vulkan
//! device, or `--no-import`): the engine copies the changed lines out of
//! shared memory and the picture is a memory texture.

use crate::app::{App, Msg};
use crate::machine::MachineUi;
use crate::vk;
use adw::prelude::*;
use gtk::{gdk, glib};
use origami_engine::{Frame, ImportedUpdate};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Samples kept for the percentiles; older ones fall off.
const SAMPLES: usize = 4096;

pub fn push_capped(v: &mut Vec<u32>, x: u32) {
    if v.len() >= SAMPLES {
        v.drain(..SAMPLES / 2);
    }
    v.push(x);
}

pub fn dmabuf_modifiers(win: &impl IsA<gtk::Widget>) -> Vec<u64> {
    let display = win.display();
    let formats = display.dmabuf_formats();
    (0..formats.n_formats())
        .map(|i| formats.format(i))
        .filter(|(fourcc, _)| *fourcc == vk::DRM_FORMAT_XRGB8888)
        .map(|(_, m)| m)
        .collect()
}

/// Take whatever the machine has drawn since last time and show it.
pub fn pump(app: &Rc<App>, m: &Rc<MachineUi>) {
    m.wake_pending
        .store(false, std::sync::atomic::Ordering::Release);
    let heads = m.machine.heads();
    if !heads.first().is_some_and(|h| h.active) {
        m.size.set((0, 0));
        m.apply_state();
        return;
    }
    let mut pixels = m.pixels.borrow_mut();
    let frame = match m.machine.take_frame(0, &mut pixels) {
        Ok(f) => f,
        Err(e) => {
            m.status.set_label(&format!("Display error: {e}"));
            m.status.set_visible(true);
            return;
        }
    };
    match frame {
        Some(Frame::Imported(f)) => {
            drop(pixels);
            if let Some(so) = m.scanout.borrow_mut().as_mut() {
                so.mark_dirty(&f.update.runs, f.update.resized);
            }
            *m.last.borrow_mut() = Some(f.clone());
            present_imported(app, m, &f);
        }
        Some(Frame::Rows(u)) => {
            // The copying fallback: rows into a full-frame buffer, then a
            // memory texture. This is the CPU path the import avoids.
            let (w, h) = (u.width as usize, u.height as usize);
            let mut full = m.fallback.borrow_mut();
            if u.resized || full.len() != w * h * 4 {
                full.clear();
                full.resize(w * h * 4, 0);
            }
            let mut at = 0;
            for &(y0, y1) in &u.runs {
                let n = (y1 - y0) as usize * w * 4;
                full[y0 as usize * w * 4..][..n].copy_from_slice(&pixels[at..at + n]);
                at += n;
                m.stats.borrow_mut().copied_rows += u64::from(y1 - y0);
            }
            drop(pixels);
            let bytes = glib::Bytes::from(&full[..]);
            let tex = gdk::MemoryTexture::new(
                w as i32,
                h as i32,
                gdk::MemoryFormat::B8g8r8x8,
                &bytes,
                w * 4,
            );
            m.set_texture(tex.upcast_ref());
            shown(m, u.width, u.height, u.seq);
        }
        None => {}
    }
}

fn shown(m: &MachineUi, w: u32, h: u32, seq: u64) {
    if m.size.replace((w, h)) != (w, h) {
        m.screen.queue_allocate();
    }
    m.seq.set(seq);
    m.last_frame.set(Instant::now());
    if !m.root.is_mapped() {
        m.stats.borrow_mut().presents_while_hidden += 1;
    }
    m.stats
        .borrow_mut()
        .first_picture
        .get_or_insert_with(Instant::now);
    m.status.set_visible(false);
}

fn present_imported(app: &Rc<App>, m: &Rc<MachineUi>, f: &ImportedUpdate) {
    let Some(dev) = &app.dev else { return };
    let (w, h) = (f.update.width, f.update.height);
    {
        let mut so = m.scanout.borrow_mut();
        if so.as_ref().is_none_or(|s| (s.width, s.height) != (w, h)) {
            *so = None;
            match vk::Scanout::new(dev, w, h, 4, &app.gtk_mods.borrow()) {
                Ok(s) => {
                    eprintln!(
                        "origami-gtk: {}: scanout {w}x{h}: {}",
                        m.name, s.modifier_note
                    );
                    *so = Some(s);
                }
                Err(e) => {
                    m.status.set_label(&format!("Cannot export dma-bufs: {e}"));
                    m.status.set_visible(true);
                    eprintln!("origami-gtk: scanout failed: {e}");
                    return;
                }
            }
        }
    }
    let pending = {
        let mut so = m.scanout.borrow_mut();
        match so.as_mut().unwrap().present(f) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("origami-gtk: present: {e}");
                return;
            }
        }
    };
    let Some(p) = pending else {
        // GTK or the compositor holds every image; try again shortly. The
        // picture is in the machine's memory, so nothing is lost by waiting.
        if !m.retry_pending.replace(true) {
            m.stats.borrow_mut().retries += 1;
            let (app, m) = (app.clone(), m.clone());
            glib::timeout_add_local_once(Duration::from_millis(4), move || {
                m.retry_pending.set(false);
                let last = m.last.borrow().clone();
                if let Some(f) = last {
                    present_imported(&app, &m, &f);
                }
            });
        }
        return;
    };
    // The GPU copy is waited for on a worker thread, so the UI thread never
    // blocks on a fence.
    worker()
        .send(Job {
            pending: p,
            dir: m.dir.clone(),
            tx: app.tx.clone(),
        })
        .ok();
}

struct Job {
    pending: vk::Pending,
    dir: std::path::PathBuf,
    tx: async_channel::Sender<Msg>,
}

fn worker() -> &'static std::sync::mpsc::Sender<Job> {
    static W: std::sync::OnceLock<std::sync::mpsc::Sender<Job>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("gpu-copy-wait".into())
            .spawn(move || {
                for mut j in rx {
                    let r = j.pending.wait();
                    let _ = j.tx.send_blocking(Msg::CopyDone(j.dir, j.pending, r));
                }
            })
            .expect("spawn gpu wait thread");
        tx
    })
}

/// A queued copy has finished on the GPU: show it, unless a newer picture is
/// already on screen or the images were replaced meanwhile.
pub fn copy_done(m: &Rc<MachineUi>, p: vk::Pending, r: Result<(), String>) {
    let mut guard = m.scanout.borrow_mut();
    let Some(so) = guard.as_mut().filter(|s| s.id == p.scanout) else {
        return;
    };
    if let Err(e) = r {
        // The GPU may still be using the image; wait it out, and make every
        // image take a full copy next time.
        so.fail(p.slot);
        eprintln!("origami-gtk: gpu copy: {e}");
        return;
    }
    if p.seq < m.seq.get() {
        so.discard(p.slot);
        return;
    }
    let ready = so.complete(&p);
    let (w, h) = (so.width, so.height);
    drop(guard);
    show_ready(m, ready, w, h, p.seq);
}

fn show_ready(m: &Rc<MachineUi>, r: vk::Ready, w: u32, h: u32, seq: u64) {
    let (free, keep) = (r.free.clone(), r.fd.clone());
    let display = m.picture.display();
    // SAFETY: the fd stays open (`keep`) until GTK drops the texture, and the
    // release function only flips a flag.
    let tex = unsafe {
        gdk::DmabufTextureBuilder::new()
            .set_display(&display)
            .set_fourcc(vk::DRM_FORMAT_XRGB8888)
            .set_modifier(r.modifier)
            .set_width(w)
            .set_height(h)
            .set_n_planes(1)
            .set_fd(0, std::os::fd::AsRawFd::as_raw_fd(&*r.fd))
            .set_stride(0, r.stride)
            .set_offset(0, r.offset)
            .build_with_release_func(move || {
                free.store(true, std::sync::atomic::Ordering::Release);
                drop(keep);
            })
    };
    match tex {
        Ok(t) => {
            m.set_texture(t.upcast_ref());
            m.shown_slot.set(Some(r.slot));
            shown(m, w, h, seq);
            let mut s = m.stats.borrow_mut();
            s.imported += 1;
            push_capped(&mut s.copy_us, r.gpu_time.as_micros() as u32);
        }
        Err(e) => {
            r.free.store(true, std::sync::atomic::Ordering::Release);
            m.status.set_label(&format!("GDK refused the dma-buf: {e}"));
            m.status.set_visible(true);
            eprintln!("origami-gtk: dmabuf texture: {e}");
        }
    }
}
