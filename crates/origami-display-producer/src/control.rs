//! The `qemu.test` extension's handlers.

use crate::engine::*;
use origami_display::v4::testext::{self, kind, op, Control, Stimulus};
use origami_display::v4::wire;

impl Engine {
    pub(crate) fn on_test_message(&mut self, vi: usize, local: u8, msg: &[u8]) {
        let ix = self.viewers[vi].test_index.unwrap();
        match local {
            testext::STIMULUS => match Stimulus::decode(msg) {
                Ok(s) => self.stimulus(&s),
                Err(e) => self.fail(vi, wire::err::PROTOCOL, e.0),
            },
            testext::STATS_REQUEST => {
                let s = self.stats_snapshot(vi);
                self.plain(vi, s.encode(ix));
            }
            testext::CONTROL => match Control::decode(msg) {
                Ok(c) => {
                    let result = self.control(vi, &c);
                    self.plain(vi, testext::control_done(ix, c.op, result));
                }
                Err(e) => self.fail(vi, wire::err::PROTOCOL, e.0),
            },
            _ => {}
        }
    }

    fn stimulus(&mut self, s: &Stimulus) {
        let h = s.head;
        let valid = self.heads.get(h as usize).is_some_and(|x| x.present);
        match s.kind {
            kind::PATTERN if valid => self.host.stimulus(s),
            kind::CURSOR_MOVE if valid => {
                let (x, y) = ((s.seed & 0xFFFF) as i32, (s.seed >> 16 & 0xFFFF) as i32);
                self.cursor_move(h, x, y, s.arg != 0);
            }
            kind::CURSOR_IMAGE if valid => {
                let n = s.arg.min(128);
                let px = (s.seed as u32).to_le_bytes();
                let pixels: Vec<u8> = (0..n * n).flat_map(|_| px).collect();
                let img = wire::CursorImage {
                    head: h,
                    image_serial: 0,
                    width: n,
                    height: n,
                    hot_x: 0,
                    hot_y: 0,
                    fourcc: wire::FOURCC_AR24,
                    stride: n * 4,
                    pixels,
                };
                self.cursor_image(h, Some(img));
            }
            kind::MODE if valid => {
                let (w, ht) = ((s.seed & 0xFFFF) as u32, (s.seed >> 16 & 0xFFFF) as u32);
                if ht <= crate::lines::MAX_LINES {
                    self.host.stimulus(s);
                    self.set_mode(h, w, ht);
                }
            }
            kind::HEAD => {
                if s.arg == 1 && h as usize == self.heads.len() {
                    let spec = self.cfg.test_head.clone();
                    if self.add_head(spec).is_ok() {
                        self.host.stimulus(s);
                    }
                } else if s.arg == 0 && valid {
                    self.remove_head(h);
                }
            }
            _ => {}
        }
    }

    fn control(&mut self, vi: usize, c: &Control) -> u32 {
        let target = if c.target_viewer == 0 {
            Some(vi)
        } else {
            self.viewer_index(c.target_viewer)
        };
        let hu = c.head as usize;
        let valid = hu < self.heads.len();
        match c.op {
            op::HOLD_SAMPLE if valid => {
                self.heads[hu].hold_next = true;
                self.heads[hu].hold_owner = Some(self.viewers[vi].id);
                0
            }
            op::RELEASE_SAMPLE if valid => match self.heads[hu].held.take() {
                Some((id, start, gen)) => {
                    if gen == self.heads[hu].mode_gen && self.heads[hu].spec.pull {
                        self.run_sample(c.head, id, start);
                    } else if gen == self.heads[hu].mode_gen {
                        let pic = self.heads[hu].held_pic.take();
                        let stride = self.heads[hu].stride as usize;
                        self.push_apply(c.head, id, start, pic.as_deref(), stride);
                    } else {
                        self.heads[hu].done_ctr = self.heads[hu].done_ctr.max(id);
                        self.evaluate(c.head);
                    }
                    0
                }
                None => {
                    self.heads[hu].hold_next = false;
                    1
                }
            },
            op::RETIRE_QUEUED if valid => {
                let Some(t) = target.filter(|&t| self.viewers[t].heads.len() > hu) else {
                    return 1;
                };
                let Some(q) = self.viewers[t].heads[hu].queued.as_ref() else {
                    return 1;
                };
                let msg_id = q.msg_id;
                let tag = self.viewers[t]
                    .out
                    .iter()
                    .find(|m| m.id == msg_id && m.sent == 0)
                    .map(|m| m.tag.clone());
                match tag {
                    Some(Tag::Frame { set, region, .. }) => {
                        self.retire_slot(c.head, set, region);
                        0
                    }
                    _ => 1,
                }
            }
            op::DELAYED_MARK if valid => {
                self.host.delay_next_mark(c.head, c.arg);
                0
            }
            op::HOLD_WRITES => match target {
                Some(t) => {
                    self.viewers[t].hold_writes = true;
                    0
                }
                None => 1,
            },
            op::RESUME_WRITES => match target {
                Some(t) => {
                    self.viewers[t].hold_writes = false;
                    0
                }
                None => 1,
            },
            _ => 1,
        }
    }
}
