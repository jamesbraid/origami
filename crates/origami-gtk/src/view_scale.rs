//! Where the guest picture sits in its view and how a view point maps back to a
//! guest pixel. The picture always fills the view, aspect kept; the View menu's
//! scales only resize the window once.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewScale {
    /// Just fill: no resize.
    Fit,
    /// One device pixel per guest pixel.
    Actual,
    /// One logical pixel per guest pixel.
    X1,
    /// Two logical pixels per guest pixel.
    X2,
}

impl ViewScale {
    pub fn command(self) -> &'static str {
        match self {
            ViewScale::Fit => "view.fit",
            ViewScale::Actual => "view.actual-size",
            ViewScale::X1 => "view.scale-1x",
            ViewScale::X2 => "view.scale-2x",
        }
    }

    pub fn from_command(id: &str) -> Option<ViewScale> {
        [
            ViewScale::Fit,
            ViewScale::Actual,
            ViewScale::X1,
            ViewScale::X2,
        ]
        .into_iter()
        .find(|s| s.command() == id)
    }

    /// Device pixels per guest pixel this scale asks for; Fit asks for none.
    fn step(self, factor: f64) -> Option<f64> {
        match self {
            ViewScale::Fit => None,
            ViewScale::Actual => Some(1.0),
            ViewScale::X1 => Some(factor),
            ViewScale::X2 => Some(2.0 * factor),
        }
    }
}

/// The picture's place in the view. The device rectangle is exact; `alloc` is
/// the whole-logical-pixel box that contains it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// Device pixels per guest pixel.
    pub step: f64,
    /// Whole steps keep every guest pixel the same size, so they are drawn unsmoothed.
    pub nearest: bool,
    pub factor: f64,
    /// Device rectangle in the view: x, y, width, height.
    pub dev: (i32, i32, i32, i32),
}

/// Place a `guest` picture in a `view` of logical pixels on a display of
/// `factor` device pixels per logical pixel: the largest size that fits.
pub fn layout(view: (i32, i32), guest: (u32, u32), factor: f64) -> Option<Layout> {
    if guest.0 == 0 || guest.1 == 0 || view.0 <= 0 || view.1 <= 0 || factor <= 0.0 {
        return None;
    }
    let (gw, gh) = (f64::from(guest.0), f64::from(guest.1));
    let (vw, vh) = (
        (f64::from(view.0) * factor).round(),
        (f64::from(view.1) * factor).round(),
    );
    let step = (vw / gw).min(vh / gh);
    let size = |g: f64, v: f64| ((g * step).round()).clamp(1.0, v) as i32;
    let (dw, dh) = (size(gw, vw), size(gh, vh));
    Some(Layout {
        step,
        nearest: step >= 1.0 && (step - step.round()).abs() < 1e-6,
        factor,
        dev: ((vw as i32 - dw) / 2, (vh as i32 - dh) / 2, dw, dh),
    })
}

/// Logical pixels kept free of the monitor's edges for panels and window
/// decorations: Wayland reports no work area.
const MARGIN: (i32, i32) = (32, 96);

/// The window size for a picture's first appearance: 1 logical pixel per
/// guest pixel if the monitor holds it, else the largest aspect-correct
/// picture that fits. Never larger than 1x. Sizes are logical; `chrome` is
/// window minus view.
pub fn first_window(monitor: (i32, i32), chrome: (i32, i32), guest: (u32, u32)) -> (i32, i32) {
    let room = room(monitor, chrome);
    let k = (f64::from(room.0) / f64::from(guest.0))
        .min(f64::from(room.1) / f64::from(guest.1))
        .min(1.0);
    let v = |g: u32, r: i32| ((f64::from(g) * k).ceil() as i32).min(r);
    (chrome.0 + v(guest.0, room.0), chrome.1 + v(guest.1, room.1))
}

/// `want`, never smaller than the window's size `now` on either axis; `None`
/// when `now` already holds it. The first fit keeps a window the user made bigger.
pub fn grow_only(now: (i32, i32), want: (i32, i32)) -> Option<(i32, i32)> {
    (want.0 > now.0 || want.1 > now.1).then(|| (want.0.max(now.0), want.1.max(now.1)))
}

/// The window size for a one-shot View scale, capped to the monitor; `None`
/// for Fit, which never resizes. Whole logical sizes can leave a fractional
/// display factor a device pixel over the exact step.
pub fn scale_window(
    scale: ViewScale,
    monitor: (i32, i32),
    chrome: (i32, i32),
    guest: (u32, u32),
    factor: f64,
) -> Option<(i32, i32)> {
    let view = view_for(guest, scale.step(factor)?, factor, room(monitor, chrome));
    Some((chrome.0 + view.0, chrome.1 + view.1))
}

fn room(monitor: (i32, i32), chrome: (i32, i32)) -> (i32, i32) {
    (
        (monitor.0 - MARGIN.0 - chrome.0).max(1),
        (monitor.1 - MARGIN.1 - chrome.1).max(1),
    )
}

fn view_for(guest: (u32, u32), step: f64, factor: f64, room: (i32, i32)) -> (i32, i32) {
    let fit = |g: u32, r: i32| ((f64::from(g) * step / factor).ceil() as i32).min(r);
    (fit(guest.0, room.0), fit(guest.1, room.1))
}

impl Layout {
    /// The logical box the picture's widget takes: x, y, width, height.
    pub fn alloc(&self) -> (i32, i32, i32, i32) {
        let f = self.factor;
        let (dx, dy, dw, dh) = self.dev;
        let (x, y) = ((f64::from(dx) / f).floor(), (f64::from(dy) / f).floor());
        let (r, b) = (
            (f64::from(dx + dw) / f).ceil(),
            (f64::from(dy + dh) / f).ceil(),
        );
        (x as i32, y as i32, (r - x) as i32, (b - y) as i32)
    }

    /// Where the picture starts inside `alloc`, in device pixels.
    pub fn offset(&self) -> (f64, f64) {
        let (x, y, _, _) = self.alloc();
        (
            f64::from(self.dev.0) - f64::from(x) * self.factor,
            f64::from(self.dev.1) - f64::from(y) * self.factor,
        )
    }

    /// The guest pixel under a view point, clamped to the picture.
    pub fn guest_at(&self, guest: (u32, u32), x: f64, y: f64) -> (i32, i32) {
        let (dx, dy, dw, dh) = self.dev;
        let c = |v: f64, o: i32, d: i32, g: u32| {
            ((v * self.factor - f64::from(o)) * f64::from(g) / f64::from(d))
                .clamp(0.0, f64::from(g) - 1.0) as i32
        };
        (c(x, dx, dw, guest.0), c(y, dy, dh, guest.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: (u32, u32) = (1280, 1024);

    #[test]
    fn a_1280_guest_is_1280_logical_pixels_at_1x_and_2x() {
        let l = layout((1280, 1024), G, 1.0).unwrap();
        assert_eq!(
            (l.alloc(), l.step, l.nearest),
            ((0, 0, 1280, 1024), 1.0, true)
        );
        let l = layout((1280, 1024), G, 2.0).unwrap();
        assert_eq!(
            (l.alloc(), l.step, l.nearest),
            ((0, 0, 1280, 1024), 2.0, true)
        );
    }

    #[test]
    fn the_picture_fills_the_view_at_any_step() {
        // 1000x700 logical is 2000x1400 device: 1.367 device px per guest px.
        let l = layout((1000, 700), G, 2.0).unwrap();
        assert!(!l.nearest && (l.step - 1.367).abs() < 0.001, "{l:?}");
        assert_eq!((l.dev.2, l.dev.3), (1750, 1400));
        assert_eq!((l.alloc().2, l.alloc().3), (876, 700));
        // 3000x2100 is 2.34, not stepped down to 2.
        let l = layout((3000, 2100), G, 1.0).unwrap();
        assert!(
            !l.nearest && (l.step - 2100.0 / 1024.0).abs() < 1e-9,
            "{l:?}"
        );
        assert_eq!(l.dev.3, 2100);
    }

    #[test]
    fn only_whole_steps_of_a_device_pixel_or_more_are_nearest() {
        assert!(layout((2560, 2048), G, 1.0).unwrap().nearest);
        assert!(!layout((2559, 2048), G, 1.0).unwrap().nearest);
        let l = layout((640, 640), G, 1.0).unwrap();
        assert_eq!((l.alloc(), l.nearest), ((0, 64, 640, 512), false));
        assert!(!layout((640, 512), G, 1.0).unwrap().nearest);
    }

    #[test]
    fn a_fractional_display_factor_fills_in_device_pixels() {
        let l = layout((1920, 1536), G, 1.5).unwrap();
        assert_eq!(l.step, 2.25);
        assert_eq!((l.dev.2, l.dev.3), (2880, 2304));
        assert_eq!(l.alloc(), (0, 0, 1920, 1536));
        assert_eq!(l.guest_at(G, 0.0, 0.0), (0, 0));
    }

    #[test]
    fn there_is_no_layout_without_a_picture() {
        assert_eq!(layout((800, 600), (0, 0), 1.0), None);
        assert_eq!(layout((0, 0), G, 1.0), None);
    }

    #[test]
    fn the_pointer_maps_through_the_picture() {
        let l = layout((1600, 1300), G, 1.0).unwrap();
        // 1.25 device px per guest px, centred vertically.
        assert_eq!((l.dev.2, l.dev.3), (1600, 1280));
        assert_eq!(l.guest_at(G, 0.0, 10.0), (0, 0));
        assert_eq!(l.guest_at(G, 800.0, 650.0), (640, 512));
        assert_eq!(l.guest_at(G, 1599.0, 1289.0), (1279, 1023));
        assert_eq!(l.guest_at(G, 0.0, 5000.0), (0, 1023));
    }

    #[test]
    fn the_first_window_is_one_logical_pixel_per_guest_pixel_when_it_fits() {
        let c = (40, 60);
        assert_eq!(first_window((1920, 1300), c, G), (40 + 1280, 60 + 1024));
        // A huge monitor never goes above 1x.
        assert_eq!(first_window((7680, 4320), c, G), (40 + 1280, 60 + 1024));
    }

    #[test]
    fn the_first_fit_only_grows_the_window() {
        assert_eq!(grow_only((1500, 1000), (1320, 1084)), Some((1500, 1084)));
        assert_eq!(grow_only((1500, 1100), (1320, 1084)), None);
    }

    #[test]
    fn a_small_monitor_gets_the_largest_picture_of_the_right_shape() {
        // 1920x1080: room is 1848x924, so 924/1024 = 0.902 and the height fills.
        let (w, h) = first_window((1920, 1080), (40, 60), G);
        assert_eq!(h, 60 + 924);
        assert_eq!(w, 40 + 1155);
        let l = layout((w - 40, h - 60), G, 1.0).unwrap();
        assert!(!l.nearest && (l.step - 0.902).abs() < 0.001, "{l:?}");
    }

    #[test]
    fn view_scales_resize_once_and_are_capped_to_the_monitor() {
        let m = (3000, 2400);
        let c = (40, 60);
        let at = |s, f| scale_window(s, m, c, G, f);
        assert_eq!(at(ViewScale::X1, 2.0), Some((40 + 1280, 60 + 1024)));
        assert_eq!(at(ViewScale::X2, 2.0), Some((40 + 2560, 60 + 2048)));
        assert_eq!(at(ViewScale::Actual, 2.0), Some((40 + 640, 60 + 512)));
        assert_eq!(at(ViewScale::Fit, 2.0), None);
        // 2x of a 1280x1024 guest at 1x does not fit 1000x700: capped.
        assert_eq!(
            scale_window(ViewScale::X2, (1000, 700), c, G, 1.0),
            Some((1000 - 32, 700 - 96))
        );
    }

    // The numbers the app's self-tests read back from the real window are the core's table.
    #[test]
    fn the_layout_matches_the_core_behaviour_table() {
        use origami_core::behaviour::{table, Action, Picture};
        let t = table();
        let mut checked = 0;
        for r in &t.rows {
            let (Some(scale), Some(pane), Picture::At(at), Action::Resize | Action::ViewFit) =
                (r.scale, r.pane, r.picture, r.action)
            else {
                continue;
            };
            let l = layout(
                (pane[0] as i32, pane[1] as i32),
                (t.guest[0], t.guest[1]),
                scale,
            )
            .unwrap();
            let got = [l.dev.0, l.dev.1, l.dev.2, l.dev.3];
            assert!(
                got.iter()
                    .zip(at)
                    .all(|(g, w)| (g - w).abs() <= t.tolerance_px),
                "{}: {got:?}, want {at:?}",
                r.id
            );
            if let Some(whole) = r.whole_pixel {
                assert_eq!(l.nearest, whole, "{}", r.id);
            }
            checked += 1;
        }
        assert!(checked >= 10);
    }

    #[test]
    fn view_scales_round_trip_through_their_commands() {
        for s in [
            ViewScale::Fit,
            ViewScale::Actual,
            ViewScale::X1,
            ViewScale::X2,
        ] {
            assert_eq!(ViewScale::from_command(s.command()), Some(s));
        }
        assert_eq!(ViewScale::from_command("view.scale-3x"), None);
    }
}
