//! Where the guest picture sits in its view and how a view point maps back to a
//! guest pixel. A guest pixel is one logical pixel at 1x (two device pixels on 2x).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewScale {
    /// Whole device steps while a guest pixel stays at least one logical
    /// pixel, else a smooth fill.
    #[default]
    Fit,
    /// One device pixel per guest pixel.
    Actual,
    /// One logical pixel per guest pixel.
    X1,
    /// Two logical pixels per guest pixel.
    X2,
}

impl ViewScale {
    pub fn parse(s: &str) -> Option<ViewScale> {
        Some(match s {
            "fit" => ViewScale::Fit,
            "actual" => ViewScale::Actual,
            "1x" => ViewScale::X1,
            "2x" => ViewScale::X2,
            _ => return None,
        })
    }

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
/// `factor` device pixels per logical pixel. A fixed scale that does not fit falls back to Fit.
pub fn layout(mode: ViewScale, view: (i32, i32), guest: (u32, u32), factor: f64) -> Option<Layout> {
    if guest.0 == 0 || guest.1 == 0 || view.0 <= 0 || view.1 <= 0 || factor <= 0.0 {
        return None;
    }
    let (gw, gh) = (f64::from(guest.0), f64::from(guest.1));
    let (vw, vh) = (
        (f64::from(view.0) * factor).round(),
        (f64::from(view.1) * factor).round(),
    );
    let fit = (vw / gw).min(vh / gh);
    let stepped = if fit.floor() >= factor {
        fit.floor()
    } else {
        fit
    };
    let want = match mode {
        ViewScale::Fit => stepped,
        ViewScale::Actual => 1.0,
        ViewScale::X1 => factor,
        ViewScale::X2 => 2.0 * factor,
    };
    let step = if want <= fit * (1.0 + 1e-9) {
        want
    } else {
        stepped
    };
    let size = |g: f64, v: f64| ((g * step).round()).clamp(1.0, v) as i32;
    let (dw, dh) = (size(gw, vw), size(gh, vh));
    Some(Layout {
        step,
        nearest: step >= 1.0 && (step - step.round()).abs() < 1e-6,
        factor,
        dev: ((vw as i32 - dw) / 2, (vh as i32 - dh) / 2, dw, dh),
    })
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

    fn rect(l: Layout) -> (i32, i32, i32, i32) {
        l.alloc()
    }

    #[test]
    fn a_1280_guest_is_1280_logical_pixels_at_1x_and_2x() {
        let l = layout(ViewScale::Fit, (1280, 1024), G, 1.0).unwrap();
        assert_eq!(
            (rect(l), l.step, l.nearest),
            ((0, 0, 1280, 1024), 1.0, true)
        );
        let l = layout(ViewScale::Fit, (1280, 1024), G, 2.0).unwrap();
        assert_eq!(
            (rect(l), l.step, l.nearest),
            ((0, 0, 1280, 1024), 2.0, true)
        );
    }

    #[test]
    fn fit_takes_whole_steps_and_centres_the_rest() {
        let l = layout(ViewScale::Fit, (3000, 2100), G, 1.0).unwrap();
        assert_eq!((rect(l), l.step), ((220, 26, 2560, 2048), 2.0));
        let l = layout(ViewScale::Fit, (1600, 1300), G, 1.0).unwrap();
        assert_eq!((rect(l), l.step), ((160, 138, 1280, 1024), 1.0));
    }

    #[test]
    fn a_small_window_on_a_2x_display_never_halves_the_picture() {
        // 1000x700 logical is 2000x1400 device: 1.37 device px per guest px.
        let l = layout(ViewScale::Fit, (1000, 700), G, 2.0).unwrap();
        assert!(!l.nearest && (l.step - 1.367).abs() < 0.001, "{l:?}");
        assert_eq!((l.dev.2, l.dev.3), (1750, 1400));
        assert_eq!((l.alloc().2, l.alloc().3), (876, 700));
    }

    #[test]
    fn a_view_too_small_for_1x_shrinks_the_picture_smoothly() {
        let l = layout(ViewScale::Fit, (640, 640), G, 1.0).unwrap();
        assert_eq!((rect(l), l.nearest), ((0, 64, 640, 512), false));
    }

    #[test]
    fn fixed_scales_count_device_or_logical_pixels() {
        let big = (3000, 2400);
        let l = layout(ViewScale::Actual, big, G, 2.0).unwrap();
        assert_eq!((rect(l), l.step), ((1180, 944, 640, 512), 1.0));
        let l = layout(ViewScale::X1, big, G, 2.0).unwrap();
        assert_eq!((l.alloc().2, l.step), (1280, 2.0));
        let l = layout(ViewScale::X2, big, G, 2.0).unwrap();
        assert_eq!((l.alloc().2, l.step), (2560, 4.0));
    }

    #[test]
    fn a_fixed_scale_that_does_not_fit_falls_back_to_fit() {
        let l = layout(ViewScale::X2, (1280, 1024), G, 1.0).unwrap();
        assert_eq!((l.alloc().2, l.step), (1280, 1.0));
    }

    #[test]
    fn a_fractional_display_factor_keeps_the_picture_in_the_view() {
        let l = layout(ViewScale::Fit, (1920, 1536), G, 1.5).unwrap();
        assert_eq!(l.step, 2.0);
        // Rounded in device pixels: exactly two device pixels per guest pixel.
        assert_eq!((l.dev.2, l.dev.3), (2560, 2048));
        assert_eq!(l.alloc(), (106, 85, 1708, 1366));
        assert_eq!(l.guest_at(G, 0.0, 0.0), (0, 0));
    }

    #[test]
    fn there_is_no_layout_without_a_picture() {
        assert_eq!(layout(ViewScale::Fit, (800, 600), (0, 0), 1.0), None);
        assert_eq!(layout(ViewScale::Fit, (0, 0), G, 1.0), None);
    }

    #[test]
    fn the_pointer_maps_through_the_scale_at_1x() {
        let l = layout(ViewScale::Fit, (1600, 1300), G, 1.0).unwrap();
        assert_eq!(l.guest_at(G, 160.0, 138.0), (0, 0));
        assert_eq!(l.guest_at(G, 800.0, 650.0), (640, 512));
        assert_eq!(l.guest_at(G, 1439.0, 1161.0), (1279, 1023));
        // Outside the picture: pinned to its edge.
        assert_eq!(l.guest_at(G, 0.0, 5000.0), (0, 1023));
    }

    #[test]
    fn the_pointer_maps_through_the_scale_at_2x() {
        // Fit on a 2x display: one guest pixel per logical pixel.
        let l = layout(ViewScale::Fit, (1280, 1024), G, 2.0).unwrap();
        assert_eq!(l.guest_at(G, 640.0, 512.0), (640, 512));
        // Actual size on 2x: a guest pixel is half a logical pixel.
        let l = layout(ViewScale::Actual, (1280, 1024), G, 2.0).unwrap();
        assert_eq!(rect(l), (320, 256, 640, 512));
        assert_eq!(l.guest_at(G, 320.0, 256.0), (0, 0));
        assert_eq!(l.guest_at(G, 640.0, 512.0), (640, 512));
        // Two logical pixels per guest pixel.
        let l = layout(ViewScale::X2, (3000, 2400), G, 1.0).unwrap();
        assert_eq!(
            l.guest_at(
                G,
                f64::from(l.alloc().0) + 101.0,
                f64::from(l.alloc().1) + 41.0
            ),
            (50, 20)
        );
    }

    #[test]
    fn view_scales_round_trip_through_their_commands_and_names() {
        for s in [
            ViewScale::Fit,
            ViewScale::Actual,
            ViewScale::X1,
            ViewScale::X2,
        ] {
            assert_eq!(ViewScale::from_command(s.command()), Some(s));
        }
        assert_eq!(ViewScale::parse("2x"), Some(ViewScale::X2));
        assert_eq!(ViewScale::parse("3x"), None);
    }
}
