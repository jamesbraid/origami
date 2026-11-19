//! The runner's display: iris's boards present through [`IrisRenderer`] into
//! the shared display protocol 4 producer.

use crate::display::{follow_cursor, Attach, Board, Hands, IrisHost, Presenter};
use crate::keys;
use iris::compositor::SwCompositor;
use iris::debug_overlay::DebugOverlay;
use iris::dev::ng1::rex3::Renderer;
use iris::disp::{BarStats, Rex3Screen, StatusBar, StatusBarTexture, FB_STRIDE};
use iris::gfx_display::GfxDisplay;
use iris::hostinput::HostInput;
use origami_display::v4::wire;
use origami_display_producer::{Config, HeadSpec, Producer};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// Serve the machine's first display on a display protocol 4 socket at `path`.
pub fn start(
    display: Arc<dyn GfxDisplay>,
    ps2: Arc<iris::dev::ps2::Ps2Controller>,
    cpu: Arc<dyn iris::gdb_stub::CpuDebug>,
    path: &Path,
) -> std::io::Result<Producer> {
    let handle = Arc::new(OnceLock::new());
    let board = display.clone();
    let cursor = display.clone();
    let attach: Attach = Box::new(move |head, presenter| {
        if head != 0 {
            return None;
        }
        *board.renderer_slot().lock() = Some(Box::new(IrisRenderer {
            presenter,
            compositor: SwCompositor::new(),
            topscan: None,
        }));
        Some(Arc::new(DisplayBoard(board.clone())) as Arc<dyn Board>)
    });
    let hands = Box::new(Keyboard(HostInput::new(Some(display), ps2)));
    let host = IrisHost::new(handle.clone(), hands, attach);
    let producer = Producer::start_at(
        Config {
            name: "iris".into(),
            steering: true,
            ..Default::default()
        },
        path,
        Box::new(host),
        Producer::waker()?,
    )?;
    let _ = handle.set(producer.handle());
    // Tested on Newport and VPro. Other boards with `hw_cursor` (Gr2, Mgras,
    // GBE) get steering untested; one without reports no cursor.
    follow_cursor(
        producer.handle(),
        0,
        move || cpu.is_running(),
        move || cursor.hw_cursor().map(|c| (c.x, c.y)),
    );
    // The head has no mode until the PROM sets one; its first present gives it.
    producer.with(|e| {
        e.add_head(HeadSpec {
            width: 0,
            height: 0,
            label: "iris".into(),
            pointer_mode: wire::POINTER_RELATIVE,
            pull: false,
            fourcc: wire::FOURCC_XB24,
            ..Default::default()
        })
    })?;
    Ok(producer)
}

struct DisplayBoard(Arc<dyn GfxDisplay>);

impl Board for DisplayBoard {
    fn request_frame(&self) {
        self.0.request_frame();
    }
}

struct Keyboard(HostInput);

impl Hands for Keyboard {
    fn key(&mut self, evdev: u32, down: bool) {
        if let Some(code) = keys::from_evdev(evdev) {
            self.0.key(code, down);
        }
    }
    fn buttons(&mut self, mask: u8) {
        self.0.set_buttons(mask);
    }
    fn nudge(&mut self, dx: i32, dy: i32) {
        self.0.nudge(dx, dy);
    }
    fn wheel(&mut self, dz: i32) {
        self.0.wheel(dz);
    }
}

/// Composes the guest's screen when a viewer waits or iris asks for a
/// readback, and says when the board proves the picture is unchanged.
struct IrisRenderer {
    presenter: Presenter,
    compositor: SwCompositor,
    topscan: Option<usize>,
}

impl Renderer for IrisRenderer {
    fn present(
        &mut self,
        screen: &mut Rex3Screen,
        _overlay: &mut DebugOverlay,
        _status: &mut StatusBar,
        _sbtex: &mut StatusBarTexture,
        _stats: &BarStats,
        need_readback: bool,
        live_fb_rgb: Option<&[u32]>,
        live_fb_aux: Option<&[u32]>,
    ) {
        let (width, height) = (screen.width, screen.height);
        // Newport renders for a status bar tick or a retrace-line change when
        // nothing else moved; a board that builds its own frame is compared.
        let quiet = !screen.prebuilt
            && screen.status_bar_only
            && Some(screen.topscan) == self.topscan
            && !need_readback;
        self.topscan = Some(screen.topscan);
        let ticket = self.presenter.begin((width as u32, height as u32), !quiet);
        if ticket.is_none() && !need_readback {
            return;
        }
        let pixels: &[u32] = if screen.prebuilt {
            &screen.fb_rgb
        } else {
            let source = screen.compositor_source_from(live_fb_rgb, live_fb_aux);
            self.compositor.compose_pixels(&source);
            self.compositor.pixels()
        };
        if need_readback && !screen.prebuilt {
            for y in 0..height {
                let row = y * FB_STRIDE;
                screen.rgba[row..row + width].copy_from_slice(&pixels[row..row + width]);
            }
        }
        if let Some(ticket) = ticket {
            self.presenter.finish(ticket, Some((pixels, FB_STRIDE)));
        }
    }
}
