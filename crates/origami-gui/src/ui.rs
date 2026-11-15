//! Windows, tabs and pop-out. Everything here runs on Slint's event-loop
//! thread; other threads reach it through [`with_ui`].
//!
//! One `Shell` window component serves as the main window (a tab bar) and as
//! a pop-out (no tab bar). A machine is shown in exactly one place: the main
//! window while it is the current tab and not popped out, otherwise its
//! pop-out. Its head textures belong to the GPU device, not to a window, so
//! moving a machine only changes which window shows the same texture, and the
//! machine, which the engine runs, never notices.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, VecModel};

use crate::core::{CursorInfo, Engine, InputEvent, MachineId, PointerButton};
use crate::gfx::{cursor_texture, Blit, CursorDraw, Gpu, HeadTexture, Presenter, ScaleMode};
use crate::keymap;
use crate::{AboutWindow, Shell, TabInfo};

/// What the run did, for `--stats-file` and the tests.
#[derive(Default)]
pub struct Counters {
    pub wakes: AtomicU64,
    pub frames_taken: AtomicU64,
    pub rows_uploaded: AtomicU64,
    pub bytes_uploaded: AtomicU64,
    pub before_renders: AtomicU64,
    pub passes: AtomicU64,
    pub popouts: AtomicU64,
    pub docks: AtomicU64,
    pub keys_sent: AtomicU64,
    pub moves_sent: AtomicU64,
    pub input_errors: AtomicU64,
}

struct Machine {
    id: MachineId,
    name: String,
    heads: Vec<Option<HeadTexture>>,
    head_sel: usize,
    mode: ScaleMode,
    popped: bool,
    pres: Presenter,
    out_img: Option<slint::Image>,
    /// Physical size of the view area this machine is drawn in.
    area: (u32, u32),
    /// The guest's cursor on the shown head, and its image on the GPU.
    cursor: Option<CursorInfo>,
    cursor_tex: Option<(wgpu::Texture, u32)>,
    pixels: Vec<u8>,
    visible: bool,
    /// Renders of the window that showed it, for the pop-out loop.
    renders: u64,
}

pub struct Ui {
    engine: Arc<dyn Engine>,
    gpu: Gpu,
    blit: Blit,
    pub counters: Arc<Counters>,
    machines: Vec<Machine>,
    main: Shell,
    pops: HashMap<usize, Shell>,
    tabs: Rc<VecModel<TabInfo>>,
    current: usize,
    /// Last size each window's view reported. The view is one component per
    /// window, so switching tabs does not change its size and so does not
    /// report again; the new tab takes the window's size from here.
    view_px: HashMap<usize, (u32, u32)>,
    pending: Arc<Mutex<HashSet<MachineId>>>,
    about: Option<AboutWindow>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Run `f` on the UI state. If the state is already borrowed (a callback
/// fired from inside a UI call), run it later from the event loop instead.
pub fn with_ui(f: impl FnOnce(&mut Ui) + 'static) {
    let f = RefCell::new(Some(f));
    UI.with(|u| {
        if let Ok(mut g) = u.try_borrow_mut() {
            if let (Some(ui), Some(f)) = (g.as_mut(), f.borrow_mut().take()) {
                f(ui);
            }
        }
    });
    if let Some(f) = f.into_inner() {
        slint::Timer::single_shot(std::time::Duration::ZERO, move || with_ui(f));
    }
}

/// Run `f` on the UI state now, if it is not already borrowed.
pub fn with_ui_now(f: impl FnOnce(&mut Ui)) {
    UI.with(|u| {
        if let Ok(mut g) = u.try_borrow_mut() {
            if let Some(ui) = g.as_mut() {
                f(ui);
            }
        }
    });
}

fn win_of(mi: usize) -> usize {
    mi + 1
}

/// Build the windows for the machines `ids` (named `names`) and register the
/// state. The engine has already started them.
pub fn install(
    engine: Arc<dyn Engine>,
    gpu: Gpu,
    machines: Vec<(MachineId, String)>,
) -> Arc<Counters> {
    let main = Shell::new().expect("main window");
    main.set_is_main(true);
    main.set_win_title("origami".into());
    let tabs = Rc::new(VecModel::from(
        machines
            .iter()
            .map(|(_, n)| TabInfo {
                name: n.clone().into(),
                popped: false,
            })
            .collect::<Vec<_>>(),
    ));
    main.set_tabs(slint::ModelRc::from(tabs.clone()));
    wire(&main, 0);
    let counters = Arc::new(Counters::default());
    let ui = Ui {
        engine,
        blit: Blit::new(&gpu),
        gpu,
        counters: counters.clone(),
        machines: machines
            .into_iter()
            .enumerate()
            .map(|(i, (id, name))| Machine {
                id,
                name,
                heads: Vec::new(),
                head_sel: 0,
                mode: ScaleMode::default(),
                popped: false,
                pres: Presenter::default(),
                out_img: None,
                area: (0, 0),
                cursor: None,
                cursor_tex: None,
                pixels: Vec::new(),
                visible: i == 0,
                renders: 0,
            })
            .collect(),
        main,
        pops: HashMap::new(),
        tabs,
        current: 0,
        view_px: HashMap::new(),
        pending: Arc::new(Mutex::new(HashSet::new())),
        about: None,
    };
    let pending = ui.pending.clone();
    let ids: Vec<(MachineId, bool)> = ui.machines.iter().map(|m| (m.id, m.visible)).collect();
    UI.with(|u| *u.borrow_mut() = Some(ui));
    // Engines call this from their own threads, so it only queues: one
    // callback on the UI thread however many wakes arrive meanwhile.
    UI.with(|u| {
        let g = u.borrow();
        let ui = g.as_ref().unwrap();
        let counters = ui.counters.clone();
        ui.engine.set_wake(Arc::new(move |id| {
            counters.wakes.fetch_add(1, Ordering::Relaxed);
            if pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id) {
                let _ = slint::invoke_from_event_loop(move || with_ui(move |u| u.poll(id)));
            }
        }));
        for (id, visible) in ids {
            ui.engine.set_visible(id, visible);
        }
    });
    counters
}

/// Release the GPU side in a defined order, here and now, while thread-locals
/// are still alive: the UI's textures, buffers and pipelines first. Left to
/// thread-local destructors at exit, wgpu's own thread-locals can already be
/// gone (debug builds keep a lock trace in one) and dropping a buffer there
/// panics inside a destructor, which aborts the process.
///
/// The device and queue are not so simple: Slint's backend holds its own
/// handles to them in a thread-local that is torn down after this returns, and
/// whoever drops the last one does it in that destructor. One handle is kept
/// on purpose, so none of them ever is; the process is about to exit and the
/// driver releases the device with it.
pub fn shutdown() {
    let ui = UI.with(|u| u.borrow_mut().take());
    if let Some(ui) = ui {
        let gpu = ui.gpu.clone();
        drop(ui);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
        std::mem::forget(gpu);
    }
}

pub fn run() {
    UI.with(|u| {
        let g = u.borrow();
        let ui = g.as_ref().unwrap();
        ui.main.show().unwrap();
    });
    with_ui(|u| {
        u.push(0);
        u.push_tabs();
        // Pictures that arrived before the window did.
        for id in u.machines.iter().map(|m| m.id).collect::<Vec<_>>() {
            u.poll(id);
        }
    });
    slint::run_event_loop_until_quit().unwrap();
}

/// Install the window's callbacks. `win` 0 is the main window; otherwise
/// 1 + the machine it holds.
fn wire(w: &Shell, win: usize) {
    let machine_of = move |u: &Ui| if win == 0 { u.current } else { win - 1 };
    w.on_tab_selected(|i| with_ui(move |u| u.select_tab(i as usize)));
    w.on_pop_out(|| {
        with_ui(|u| {
            let mi = u.current;
            u.popout(mi)
        })
    });
    w.on_about(|| with_ui(|u| u.show_about()));
    w.on_dock(move || with_ui(move |u| u.dock(win - 1)));
    w.on_set_scale(move |v| {
        with_ui(move |u| {
            let mi = machine_of(u);
            u.machines[mi].mode = if v == 1 {
                ScaleMode::Integer
            } else {
                ScaleMode::Fit
            };
            u.refresh(mi);
        })
    });
    w.on_next_head(move || {
        with_ui(move |u| {
            let mi = machine_of(u);
            let n = u.machines[mi].heads.len().max(1);
            u.machines[mi].head_sel = (u.machines[mi].head_sel + 1) % n;
            u.refresh(mi);
        })
    });
    w.on_key_event(move |text, down| {
        let text = text.to_string();
        with_ui(move |u| {
            if let (Some(mi), Some(key)) = (u.machine_in(win), keymap::from_text(&text)) {
                u.send(mi, InputEvent::Key { key, down });
                u.counters.keys_sent.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    w.on_moved(move |x, y| {
        with_ui(move |u| {
            let Some(mi) = u.machine_in(win) else { return };
            // The pointer is where the user's is, in the head's pixels; the
            // engine moves the guest's cursor there.
            if let Some((hx, hy)) = u.guest_pos(mi, x, y) {
                let head = u.machines[mi].head_sel as u32;
                u.counters.moves_sent.fetch_add(1, Ordering::Relaxed);
                u.send(mi, InputEvent::PointerAt { head, x: hx, y: hy });
            }
        })
    });
    w.on_button_event(move |b, down| {
        with_ui(move |u| {
            if let Some(mi) = u.machine_in(win) {
                let button = match b {
                    0 => PointerButton::Left,
                    1 => PointerButton::Middle,
                    _ => PointerButton::Right,
                };
                u.send(mi, InputEvent::Button { button, down });
            }
        })
    });
    w.on_wheel(move |n| {
        with_ui(move |u| {
            if let (Some(mi), true) = (u.machine_in(win), n != 0) {
                u.send(mi, InputEvent::Wheel { notches: n });
            }
        })
    });
    w.on_pointer_left(|| {});
    w.on_view_size(move |ww, hh| {
        with_ui(move |u| u.view_size(win, ww.max(0) as u32, hh.max(0) as u32))
    });
    w.window()
        .set_rendering_notifier(move |state, _| {
            if let slint::RenderingState::BeforeRendering = state {
                with_ui(move |u| u.before_render(win));
            }
        })
        .expect("rendering notifier");
    if win == 0 {
        w.window().on_close_requested(|| {
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        });
    } else {
        // Closing a pop-out docks it; the machine keeps running.
        w.window().on_close_requested(move || {
            with_ui(move |u| u.dock(win - 1));
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
}

impl Ui {
    /// The machine `win` shows right now, if any.
    fn machine_in(&self, win: usize) -> Option<usize> {
        if win == 0 {
            (!self.machines[self.current].popped).then_some(self.current)
        } else {
            self.machines
                .get(win - 1)
                .filter(|m| m.popped)
                .map(|_| win - 1)
        }
    }

    /// The window that shows machine `mi`, if any.
    fn win_for(&self, mi: usize) -> Option<usize> {
        if self.machines[mi].popped {
            Some(win_of(mi))
        } else if self.current == mi {
            Some(0)
        } else {
            None
        }
    }

    fn shell_for(&self, mi: usize) -> Option<&Shell> {
        match self.win_for(mi)? {
            0 => Some(&self.main),
            w => self.pops.get(&(w - 1)),
        }
    }

    fn send(&mut self, mi: usize, event: InputEvent) {
        if let Err(e) = self.engine.send_input(self.machines[mi].id, event) {
            if self.counters.input_errors.fetch_add(1, Ordering::Relaxed) == 0 {
                eprintln!("origami-gui: input to {}: {e}", self.machines[mi].name);
            }
        }
    }

    /// Output pixels for each guest pixel, as the shader scales: a whole
    /// number in integer mode when the picture fits at 1x, else the fit.
    fn guest_scale(&self, mi: usize) -> f32 {
        let m = &self.machines[mi];
        let Some(Some(h)) = m.heads.get(m.head_sel) else {
            return 1.0;
        };
        let fit = (m.area.0 as f32 / h.width as f32).min(m.area.1 as f32 / h.height as f32);
        match m.mode {
            ScaleMode::Integer if fit >= 1.0 => fit.floor(),
            _ => fit.max(0.01),
        }
    }

    /// The head pixel under view position (`x`, `y`) in physical pixels, or
    /// None outside the picture. Mirrors the shader's placement: the picture
    /// is centred with a floored origin.
    fn guest_pos(&self, mi: usize, x: f32, y: f32) -> Option<(i32, i32)> {
        let m = &self.machines[mi];
        let Some(Some(h)) = m.heads.get(m.head_sel) else {
            return None;
        };
        let s = self.guest_scale(mi);
        let org_x = ((m.area.0 as f32 - h.width as f32 * s) / 2.0).floor();
        let org_y = ((m.area.1 as f32 - h.height as f32 * s) / 2.0).floor();
        let (gx, gy) = (((x - org_x) / s).floor(), ((y - org_y) / s).floor());
        (gx >= 0.0 && gy >= 0.0 && gx < h.width as f32 && gy < h.height as f32)
            .then_some((gx as i32, gy as i32))
    }

    fn show_about(&mut self) {
        if self.about.is_none() {
            let w = AboutWindow::new().expect("about window");
            // Closing it only hides it.
            self.about = Some(w);
        }
        if let Some(w) = &self.about {
            let _ = w.show();
        }
    }

    fn push_tabs(&self) {
        for (i, m) in self.machines.iter().enumerate() {
            self.tabs.set_row_data(
                i,
                TabInfo {
                    name: m.name.clone().into(),
                    popped: m.popped,
                },
            );
        }
        self.main.set_current_tab(self.current as i32);
    }

    /// Copy machine `mi`'s state into the windows that describe it.
    fn push(&self, mi: usize) {
        let m = &self.machines[mi];
        // The main window always describes the current tab, popped or not.
        let mut shells: Vec<&Shell> = Vec::new();
        if m.popped {
            shells.extend(self.pops.get(&mi));
        }
        if self.current == mi {
            shells.push(&self.main);
        }
        let head = m.heads.get(m.head_sel).and_then(|h| h.as_ref());
        let status = match head {
            _ if !self.engine.is_running(m.id) => "the machine has stopped".to_string(),
            Some(h) => {
                let s = self.guest_scale(mi);
                format!("{}x{}, shown at {:.2}x", h.width, h.height, s)
            }
            None => "waiting for the guest's display".to_string(),
        };
        for w in shells {
            w.set_label(m.name.clone().into());
            w.set_popped(m.popped);
            w.set_img(m.out_img.clone().unwrap_or_default());
            w.set_guest_cursor(m.cursor.as_ref().is_some_and(|c| c.on));
            w.set_scale_mode(if m.mode == ScaleMode::Integer { 1 } else { 0 });
            w.set_head_count(m.heads.len() as i32);
            w.set_head_sel(m.head_sel as i32);
            w.set_status(status.clone().into());
            if !w.get_is_main() {
                w.set_win_title(format!("{} - pop-out", m.name).into());
            }
        }
    }

    fn redraw(&self, mi: usize) {
        if let Some(w) = self.shell_for(mi) {
            w.window().request_redraw();
        }
    }

    /// Re-describe a machine after a setting changed and draw it again.
    fn refresh(&mut self, mi: usize) {
        self.push(mi);
        self.redraw(mi);
    }

    /// Take what machine `id` has new and put it on the GPU.
    pub fn poll(&mut self, id: MachineId) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        let Some(mi) = self.machines.iter().position(|m| m.id == id) else {
            return;
        };
        if !self.machines[mi].visible {
            // The engine keeps what changed until someone looks again.
            return;
        }
        let Ok(infos) = self.engine.heads(id) else {
            return;
        };
        let mut changed_sel = false;
        let m = &mut self.machines[mi];
        if m.heads.len() != infos.len() {
            m.heads.resize_with(infos.len(), || None);
            changed_sel = true;
        }
        for (hi, info) in infos.iter().enumerate() {
            if !info.active {
                continue;
            }
            // A head is current when the texture already shows its sequence.
            if m.heads[hi].as_ref().is_some_and(|h| h.seq >= info.seq) {
                continue;
            }
            let mut pixels = std::mem::take(&mut m.pixels);
            match self.engine.take_frame(id, hi, &mut pixels) {
                Ok(Some(update)) => {
                    let c = &self.counters;
                    c.frames_taken.fetch_add(1, Ordering::Relaxed);
                    c.rows_uploaded
                        .fetch_add(u64::from(update.lines()), Ordering::Relaxed);
                    c.bytes_uploaded
                        .fetch_add(pixels.len() as u64, Ordering::Relaxed);
                    let old = m.heads[hi].take();
                    let before = old.as_ref().map(|h| h.generation);
                    let new = HeadTexture::apply(&self.gpu, old, &update, &pixels);
                    changed_sel |= before != Some(new.generation);
                    m.heads[hi] = Some(new);
                }
                Ok(None) => {}
                Err(e) => eprintln!("origami-gui: {}: {e}", m.name),
            }
            m.pixels = pixels;
        }
        if changed_sel {
            self.push(mi);
        }
        self.refresh_cursor(mi);
        self.redraw(mi);
    }

    /// Ask the engine for the guest's cursor on the shown head and keep the
    /// image on the GPU. True when something about it changed.
    fn refresh_cursor(&mut self, mi: usize) -> bool {
        let id = self.machines[mi].id;
        let head = self.machines[mi].head_sel;
        let known = self.machines[mi].cursor_tex.as_ref().map_or(0, |t| t.1);
        let Some(mut info) = self.engine.cursor(id, head, known) else {
            return false;
        };
        let m = &mut self.machines[mi];
        if let Some(image) = info.image.take() {
            let tex = m
                .cursor_tex
                .take()
                .map_or_else(|| cursor_texture(&self.gpu), |t| t.0);
            let w = info.width as usize;
            for (y, row) in image.chunks(w * 4).enumerate() {
                self.gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: y as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    row,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row.len() as u32),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: info.width,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                );
            }
            m.cursor_tex = Some((tex, info.gen));
        }
        let changed = m.cursor.as_ref() != Some(&info);
        let shown_changed = m.cursor.as_ref().is_none_or(|c| c.on) != info.on;
        m.cursor = Some(info);
        if shown_changed {
            // Whether the host cursor hides depends on it.
            self.push(mi);
        }
        changed
    }

    fn set_visible(&mut self, mi: usize, visible: bool) {
        if self.machines[mi].visible != visible {
            self.machines[mi].visible = visible;
            self.engine.set_visible(self.machines[mi].id, visible);
        }
    }

    fn update_visibility(&mut self) {
        for mi in 0..self.machines.len() {
            let v = self.machines[mi].popped || self.current == mi;
            self.set_visible(mi, v);
        }
    }

    pub fn select_tab(&mut self, i: usize) {
        if i >= self.machines.len() || i == self.current {
            return;
        }
        self.current = i;
        self.main.set_current_tab(i as i32);
        self.update_visibility();
        self.adopt_area(i);
        self.push(i);
        let id = self.machines[i].id;
        self.poll(id);
        self.redraw(i);
    }

    fn view_size(&mut self, win: usize, w: u32, h: u32) {
        self.view_px.insert(win, (w, h));
        // A hidden window's callbacks (the main window while its machine is
        // popped out) must not resize that machine's output.
        if let Some(m) = self.machine_in(win) {
            self.adopt_area(m);
        }
    }

    /// Give machine `mi` the size of the window that shows it.
    fn adopt_area(&mut self, mi: usize) {
        let Some(win) = self.win_for(mi) else { return };
        let Some(&(w, h)) = self.view_px.get(&win) else {
            return;
        };
        let m = &mut self.machines[mi];
        if m.area == (w, h) && m.out_img.is_some() {
            return;
        }
        m.area = (w, h);
        if let Some(t) = m.pres.resize(&self.gpu, w, h) {
            m.out_img = slint::Image::try_from(t).ok();
        }
        let id = m.id;
        self.engine.set_size(id, m.head_sel, w, h);
        self.push(mi);
        self.redraw(mi);
    }

    pub fn popout(&mut self, mi: usize) {
        if self.machines[mi].popped {
            return;
        }
        self.counters.popouts.fetch_add(1, Ordering::Relaxed);
        let w = Shell::new().expect("pop-out window");
        w.set_is_main(false);
        wire(&w, win_of(mi));
        self.machines[mi].popped = true;
        self.machines[mi].area = (0, 0);
        self.view_px.remove(&win_of(mi));
        self.pops.insert(mi, w);
        self.update_visibility();
        self.push(mi);
        self.push_tabs();
        self.pops[&mi].show().expect("show pop-out");
    }

    pub fn dock(&mut self, mi: usize) {
        if !self.machines[mi].popped {
            return;
        }
        self.counters.docks.fetch_add(1, Ordering::Relaxed);
        if let Some(w) = self.pops.remove(&mi) {
            let _ = w.hide();
        }
        self.machines[mi].popped = false;
        self.view_px.remove(&win_of(mi));
        self.machines[mi].area = (0, 0);
        self.update_visibility();
        self.adopt_area(mi);
        self.push_tabs();
        // The main window's view reappears; its last size is still known.
        self.push(mi);
        self.redraw(mi);
    }

    fn before_render(&mut self, win: usize) {
        self.counters.before_renders.fetch_add(1, Ordering::Relaxed);
        let Some(mi) = self.machine_in(win) else {
            return;
        };
        self.machines[mi].renders += 1;
        let m = &mut self.machines[mi];
        let Some(Some(head)) = m.heads.get(m.head_sel) else {
            return;
        };
        let draw = match (&m.cursor, &m.cursor_tex) {
            (Some(c), Some((tex, gen))) if c.on && *gen == c.gen => Some(CursorDraw {
                texture: tex,
                x: c.x - c.hot_x,
                y: c.y - c.hot_y,
                width: c.width,
                height: c.height,
                gen: c.gen,
            }),
            _ => None,
        };
        if m.pres
            .run(&self.gpu, &self.blit, head, m.mode, draw.as_ref())
        {
            self.counters.passes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Renders of the window that shows machine `mi`, for the pop-out loop.
    pub fn renders(&self, mi: usize) -> u64 {
        self.machines[mi].renders
    }

    pub fn machine_count(&self) -> usize {
        self.machines.len()
    }

    pub fn gpu_errors(&self) -> u64 {
        self.gpu.errors.count()
    }
}
