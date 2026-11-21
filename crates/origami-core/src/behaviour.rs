//! What the machine window must show, as data: one table every app's self-test
//! drives through the real window and reads back. A row is a situation (display
//! scale, the size of the picture's pane, an action) and what must result, in
//! device pixels. The numbers are written out, not computed by the code under
//! test.
//!
//! The pane is the area the picture may fill, in logical pixels; each app
//! reports its own. The picture always fills it, aspect kept.

use serde::Deserialize;

/// The table as JSON: `{"guest": [w, h], "tolerance_px": n, "rows": [...]}`.
/// The guest is an Octane2's 1280 by 1024; a measured device-pixel value may
/// be `tolerance_px` away from a row's, for rounding.
pub const JSON: &str = include_str!("behaviour.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// The window is resized until the pane has the row's size; nothing else.
    Resize,
    /// The View menu's items.
    ViewFit,
    ViewActual,
    View1x,
    View2x,
    /// The window's zoom button.
    Zoom,
    /// The machine's tab is left and come back to.
    Reattach,
    /// The machine is stopped and started, so a new run's first picture arrives in a window of the row's pane.
    FirstWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Picture {
    /// Exactly this rectangle in the pane, device pixels: x, y, width, height.
    At([i32; 4]),
    /// The largest aspect-correct picture the pane holds, whatever its size.
    Fills,
    /// What it was before the action.
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Frame {
    /// The window frame is the one it had before the action.
    Same,
    /// The window frame is a different size.
    Changes,
    /// Either: the window may already have been the size the action asks for.
    Any,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Row {
    pub id: String,
    /// Display scale (device pixels per logical pixel) the row is for; `None`
    /// for any. A run takes the rows at its display's scale.
    pub scale: Option<f64>,
    /// A View menu item chosen before the pane is set, to show the choice is not kept.
    pub view_before: Option<Action>,
    /// The pane size, logical pixels, the window is resized to first.
    pub pane: Option<[u32; 2]>,
    pub action: Action,
    /// The pane room the screen must offer, logical pixels, for the row to
    /// apply; a smaller screen skips it and the self-test says so.
    pub room: Option<[u32; 2]>,
    pub picture: Picture,
    /// Whether every guest pixel is a whole number of device pixels (a step of
    /// 1 or more that is an integer); `None` when the row does not say.
    pub whole_pixel: Option<bool>,
    pub frame: Frame,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Table {
    pub guest: [u32; 2],
    pub tolerance_px: i32,
    pub rows: Vec<Row>,
}

pub fn table() -> Table {
    serde_json::from_str(JSON).expect("behaviour.json is well formed")
}

/// A row's id, the display it ran on and what it read back, for [`check`].
/// Pictures are `[x, y, w, h]` in device pixels of the pane, sizes `[w, h]`.
#[derive(Debug, Deserialize)]
pub struct Measured {
    pub id: String,
    pub scale: f64,
    /// The pane after the action.
    pub pane_dev: [i32; 2],
    pub picture_before: [i32; 4],
    pub picture: [i32; 4],
    /// The renderer's surface and the picture's rectangle in it.
    pub surface: [i32; 2],
    pub rendered: [i32; 4],
    pub frame_before: [i32; 2],
    pub frame: [i32; 2],
    /// The same read back a second later: nothing may have moved.
    pub later: Option<Later>,
    /// Whether the app's own redraw ran after the action; `None` when the
    /// test had to force a draw because the window is occluded.
    pub drew: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct Later {
    pub frame: [i32; 2],
    pub picture: [i32; 4],
}

/// Why `id` cannot run here, or `None`: another display scale, a screen too
/// small for the pane the row needs (`room_available`, logical pixels), or a
/// window that would not reach the pane the row needs (`pane_reached` false).
pub fn skip(
    id: &str,
    scale: f64,
    room_available: Option<[i64; 2]>,
    pane_reached: bool,
) -> Option<String> {
    let t = table();
    let row = t.rows.iter().find(|r| r.id == id)?;
    if row.scale.is_some_and(|s| (s - scale).abs() > 0.01) {
        return Some(format!(
            "display scale {}, this is {scale}",
            row.scale.unwrap_or(0.0)
        ));
    }
    match (row.room, room_available) {
        (Some(need), Some(have))
            if i64::from(need[0]) > have[0] || i64::from(need[1]) > have[1] =>
        {
            return Some("the room is too small".into());
        }
        _ => {}
    }
    // The pane the row starts from, or the smaller one an action shrinks the window to: a window has a minimum size.
    (!pane_reached).then(|| "the window cannot get to the pane the row needs".to_string())
}

/// What is wrong with a row's measurements; empty when the window did what the table says.
pub fn check(m: &Measured) -> Vec<String> {
    let t = table();
    let Some(row) = t.rows.iter().find(|r| r.id == m.id) else {
        return vec![format!("no row named {}", m.id)];
    };
    // A window is whole device pixels and a pane is logical: a resize can be a pixel or two off.
    let tol = t.tolerance_px
        + if matches!(row.action, Action::Resize | Action::ViewFit) {
            m.scale.ceil() as i32
        } else {
            0
        };
    let near = |a: i32, b: i32| (a - b).abs() <= tol;
    let near4 = |a: [i32; 4], b: [i32; 4]| a.iter().zip(b).all(|(x, y)| near(*x, y));
    let mut out = Vec::new();
    if !(near(m.rendered[2], m.surface[0])
        && near(m.rendered[3], m.surface[1])
        && near(m.rendered[0], 0)
        && near(m.rendered[1], 0))
    {
        out.push(format!(
            "the renderer's picture {:?} does not fill its surface {:?}",
            m.rendered, m.surface
        ));
    }
    let (g, p) = (t.guest.map(f64::from), m.picture);
    match row.picture {
        Picture::At(want) if !near4(p, want) => out.push(format!("picture {p:?}, want {want:?}")),
        Picture::Fills => {
            let step = (f64::from(m.pane_dev[0]) / g[0]).min(f64::from(m.pane_dev[1]) / g[1]);
            let want = [(g[0] * step).round() as i32, (g[1] * step).round() as i32];
            if !(near(p[2], want[0]) && near(p[3], want[1])) {
                out.push(format!(
                    "picture {}x{} does not fill the {:?} pane ({want:?})",
                    p[2], p[3], m.pane_dev
                ));
            }
        }
        Picture::Unchanged if !near4(p, m.picture_before) => {
            out.push(format!("picture {p:?}, was {:?}", m.picture_before));
        }
        _ => {}
    }
    if let Some(want) = row.whole_pixel {
        let step = f64::from(p[2]) / g[0];
        let whole = step >= 1.0 && (step - step.round()).abs() < 0.002;
        if whole != want {
            out.push(format!("whole pixel {whole}, want {want}"));
        }
    }
    let changed = m.frame != m.frame_before;
    if row.frame != Frame::Any && (row.frame == Frame::Changes) != changed {
        out.push(format!(
            "window frame {:?} -> {:?}, want {}",
            m.frame_before,
            m.frame,
            if row.frame == Frame::Changes {
                "a change"
            } else {
                "no change"
            }
        ));
    }
    if let Some(l) = &m.later {
        if l.frame != m.frame || l.picture != m.picture {
            out.push(format!(
                "popped after settling: {:?} -> {:?}",
                m.picture, l.picture
            ));
        }
    }
    // A picture that moved was drawn again by the app's own redraw, not by the test.
    if m.drew == Some(false) && !near4(p, m.picture_before) {
        out.push("the app did not redraw after the picture moved".into());
    }
    out
}

/// The drivers' one entry: `{"op":"skip",...}` or `{"op":"check",...}` in,
/// `{"skip":reason|null}` or `{"problems":[...]}` out.
pub fn evaluate(request: &str) -> String {
    #[derive(Deserialize)]
    #[serde(tag = "op", rename_all = "kebab-case")]
    enum Request {
        Skip {
            id: String,
            scale: f64,
            room_available: Option<[i64; 2]>,
            pane_reached: bool,
        },
        Check(Box<Measured>),
    }
    match serde_json::from_str::<Request>(request) {
        Ok(Request::Skip {
            id,
            scale,
            room_available,
            pane_reached,
        }) => serde_json::json!({ "skip": skip(&id, scale, room_available, pane_reached) })
            .to_string(),
        Ok(Request::Check(m)) => serde_json::json!({ "problems": check(&m) }).to_string(),
        Err(e) => serde_json::json!({ "problems": [format!("bad request: {e}")] }).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picture a pane of `pane` logical pixels at `scale` holds: x, y, width, height, step.
    fn fill(guest: [u32; 2], pane: [u32; 2], scale: f64) -> ([i32; 4], f64) {
        let (vw, vh) = (
            (f64::from(pane[0]) * scale).round(),
            (f64::from(pane[1]) * scale).round(),
        );
        let step = (vw / f64::from(guest[0])).min(vh / f64::from(guest[1]));
        let w = (f64::from(guest[0]) * step).round();
        let h = (f64::from(guest[1]) * step).round();
        (
            [
                ((vw - w) / 2.0) as i32,
                ((vh - h) / 2.0) as i32,
                w as i32,
                h as i32,
            ],
            step,
        )
    }

    // A wrong literal in the table would make every app fail the same way: check
    // each one is the aspect-correct fill of the pane the row ends with.
    #[test]
    fn rows_state_the_fill_of_their_pane() {
        let t = table();
        for r in &t.rows {
            let (Some(scale), Picture::At(at)) = (r.scale, r.picture) else {
                continue;
            };
            // Actual Size, 1x and the first window size the window to the picture: the pane is the room named.
            let pane = match r.action {
                Action::ViewActual | Action::View1x | Action::FirstWindow => r.room,
                _ => r.pane,
            };
            let (want, step) = fill(t.guest, pane.expect("a pane or room"), scale);
            assert_eq!(at, want, "{}", r.id);
            if let Some(whole) = r.whole_pixel {
                assert_eq!(
                    whole,
                    step >= 1.0 && (step - step.round()).abs() < 1e-9,
                    "{}",
                    r.id
                );
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = table().rows.iter().map(|r| r.id.clone()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    fn measured(
        id: &str,
        scale: f64,
        picture: [i32; 4],
        pane: [i32; 2],
        frame: [i32; 2],
    ) -> Measured {
        Measured {
            id: id.into(),
            scale,
            pane_dev: pane,
            picture_before: picture,
            picture,
            surface: [picture[2], picture[3]],
            rendered: [0, 0, picture[2], picture[3]],
            frame_before: frame,
            frame,
            later: None,
            drew: Some(true),
        }
    }

    #[test]
    fn a_window_that_does_what_the_row_says_has_no_problems() {
        let m = measured(
            "fill-1x-wide-pane",
            1.0,
            [37, 0, 625, 500],
            [700, 500],
            [1000, 700],
        );
        assert_eq!(check(&m), Vec::<String>::new());
    }

    #[test]
    fn each_kind_of_wrong_window_is_named() {
        let mut m = measured(
            "fill-1x-wide-pane",
            1.0,
            [0, 0, 700, 500],
            [700, 500],
            [1000, 700],
        );
        assert!(check(&m)[0].contains("picture"), "{:?}", check(&m));
        m = measured(
            "fill-1x-wide-pane",
            1.0,
            [37, 0, 625, 500],
            [700, 500],
            [1000, 700],
        );
        m.frame = [1100, 700];
        assert!(check(&m)[0].contains("window frame"));
        m.frame = m.frame_before;
        m.later = Some(Later {
            frame: [1000, 700],
            picture: [0, 0, 640, 500],
        });
        assert!(check(&m)[0].contains("popped"));
        m.later = None;
        m.rendered = [0, 0, 500, 500];
        assert!(check(&m)[0].contains("surface"));
        m.rendered = [0, 0, 625, 500];
        m.drew = Some(false);
        assert!(
            check(&m).is_empty(),
            "nothing moved, so nothing was owed a redraw"
        );
        m.picture_before = [0, 0, 700, 500];
        assert!(check(&m)[0].contains("redraw"));
        let whole = measured(
            "fill-1x-whole-size",
            1.0,
            [0, 0, 1000, 800],
            [1000, 800],
            [1000, 700],
        );
        assert!(check(&whole).iter().any(|p| p.contains("whole pixel")));
        let fills = measured(
            "zoom-fills-the-new-window",
            1.0,
            [0, 0, 600, 400],
            [800, 600],
            [900, 700],
        );
        assert!(check(&fills).iter().any(|p| p.contains("does not fill")));
    }

    #[test]
    fn rows_skip_for_scale_room_and_an_unreachable_pane() {
        assert!(skip("fill-2x-small-pane", 1.0, None, true)
            .unwrap()
            .contains("display scale"));
        assert_eq!(skip("fill-2x-small-pane", 2.0, None, true), None);
        assert!(skip("fill-1x-whole-size", 1.0, Some([900, 900]), true)
            .unwrap()
            .contains("room"));
        assert!(skip("fill-1x-wide-pane", 1.0, None, false)
            .unwrap()
            .contains("cannot get"));
        let out = evaluate(
            r#"{"op":"skip","id":"fill-1x-wide-pane","scale":1.0,"room_available":null,"pane_reached":true}"#,
        );
        assert_eq!(out, r#"{"skip":null}"#);
    }
}
