//! Input reports, ownership and bounds (section 13).

use crate::engine::*;
use origami_display::v4::testext::{echo, InputEcho};
use origami_display::v4::wire::{self, ToProducer};

const MAX_REPORT: u32 = 256;
const REL_LIMIT: i64 = 32_767;

impl Engine {
    pub(crate) fn on_input(&mut self, vi: usize, msg: ToProducer) {
        use ToProducer as M;
        match msg {
            M::InputClaim { claim } => return self.claim(vi, claim),
            M::InputReset => {
                if self.owner == Some(self.viewers[vi].id) {
                    self.release_input(vi);
                }
                return;
            }
            _ => {}
        }
        if self.owner != Some(self.viewers[vi].id)
            || self.viewers[vi].features & wire::feat::INPUT == 0
        {
            return;
        }
        match msg {
            M::Key { head, code, down } => {
                if !self.valid_head(head) || !(1..=0x2ff).contains(&code) || down > 1 {
                    return;
                }
                self.flush_rel(vi);
                let v = &mut self.viewers[vi];
                if down == 1 {
                    v.held_keys.insert((head, code));
                } else {
                    v.held_keys.remove(&(head, code));
                }
                self.inject(vi, InputEvent::Key { head, code, down });
                self.counted(vi);
            }
            M::Button { head, button, down } => {
                if !self.valid_head(head) || button > 8 || down > 1 {
                    return;
                }
                self.flush_rel(vi);
                self.end_report(vi);
                let v = &mut self.viewers[vi];
                if down == 1 {
                    v.held_buttons.insert((head, button));
                } else {
                    v.held_buttons.remove(&(head, button));
                }
                self.inject(vi, InputEvent::Button { head, button, down });
                self.viewers[vi].report_events = 1;
                self.end_report(vi);
            }
            M::Rel { head, dx, dy } => {
                if !self.valid_head(head) {
                    return;
                }
                let v = &mut self.viewers[vi];
                match &mut v.pending_rel {
                    Some((h, x, y)) if *h == head => {
                        *x += dx as i64;
                        *y += dy as i64;
                    }
                    _ => {
                        self.flush_rel(vi);
                        self.viewers[vi].pending_rel = Some((head, dx as i64, dy as i64));
                    }
                }
            }
            M::Abs { head, x, y } => {
                if !self.valid_head(head) {
                    return;
                }
                let hd = &self.heads[head as usize];
                if hd.w == 0 || hd.spec.pointer_mode == wire::POINTER_RELATIVE {
                    return;
                }
                let (x, y) = (x.clamp(0, hd.w as i32 - 1), y.clamp(0, hd.h as i32 - 1));
                self.flush_rel(vi);
                self.inject(vi, InputEvent::Abs { head, x, y });
                self.counted(vi);
            }
            M::Sync { flags, serial } => {
                self.flush_rel(vi);
                self.end_report(vi);
                if flags & 1 != 0 {
                    self.plain(vi, wire::input_ack(serial));
                }
            }
            _ => {}
        }
    }

    fn valid_head(&self, head: u32) -> bool {
        self.heads.get(head as usize).is_some_and(|h| h.present)
    }

    fn counted(&mut self, vi: usize) {
        self.viewers[vi].report_events += 1;
        if self.viewers[vi].report_events >= MAX_REPORT {
            self.end_report(vi);
        }
    }

    fn flush_rel(&mut self, vi: usize) {
        if let Some((head, x, y)) = self.viewers[vi].pending_rel.take() {
            let (dx, dy) = (
                x.clamp(-REL_LIMIT, REL_LIMIT) as i32,
                y.clamp(-REL_LIMIT, REL_LIMIT) as i32,
            );
            self.inject(vi, InputEvent::Rel { head, dx, dy });
            self.counted(vi);
        }
    }

    fn end_report(&mut self, vi: usize) {
        if std::mem::take(&mut self.viewers[vi].report_events) > 0 {
            self.inject(vi, InputEvent::ReportEnd);
        }
    }

    /// Hand one event to the host and echo it to test viewers.
    fn inject(&mut self, _from: usize, ev: InputEvent) {
        self.host.inject(&ev);
        self.echo_seq += 1;
        let mut e = InputEcho {
            seq: self.echo_seq,
            ..Default::default()
        };
        match ev {
            InputEvent::Key { head, code, down } => {
                (e.head, e.kind, e.code, e.a) = (head, echo::KEY, code, down as i32)
            }
            InputEvent::Button { head, button, down } => {
                (e.head, e.kind, e.code, e.a) = (head, echo::BUTTON, button, down as i32)
            }
            InputEvent::Rel { head, dx, dy } => {
                (e.head, e.kind, e.a, e.b) = (head, echo::REL, dx, dy)
            }
            InputEvent::Abs { head, x, y } => (e.head, e.kind, e.a, e.b) = (head, echo::ABS, x, y),
            InputEvent::ReportEnd => e.kind = echo::REPORT_END,
        }
        for vj in 0..self.viewers.len() {
            if let Some(ix) = self.viewers[vj]
                .test_index
                .filter(|_| self.viewers[vj].live)
            {
                self.plain(vj, e.encode(ix));
            }
        }
    }

    /// End the viewer's reports and release everything it holds down.
    pub(crate) fn release_input(&mut self, vi: usize) {
        self.flush_rel(vi);
        let keys: Vec<(u32, u32)> = std::mem::take(&mut self.viewers[vi].held_keys)
            .into_iter()
            .collect();
        let buttons: Vec<(u32, u32)> = std::mem::take(&mut self.viewers[vi].held_buttons)
            .into_iter()
            .collect();
        for (head, code) in keys {
            self.inject(
                vi,
                InputEvent::Key {
                    head,
                    code,
                    down: 0,
                },
            );
            self.viewers[vi].report_events += 1;
        }
        for (head, button) in buttons {
            self.inject(
                vi,
                InputEvent::Button {
                    head,
                    button,
                    down: 0,
                },
            );
            self.viewers[vi].report_events += 1;
        }
        self.end_report(vi);
    }

    fn claim(&mut self, vi: usize, claim: bool) {
        let id = self.viewers[vi].id;
        if self.viewers[vi].features & wire::feat::INPUT == 0 {
            return;
        }
        let prev = self.owner;
        if claim && prev != Some(id) {
            if let Some(pi) = prev.and_then(|p| self.viewer_index(p)) {
                self.release_input(pi);
            }
            self.owner = Some(id);
        } else if !claim && prev == Some(id) {
            self.release_input(vi);
            self.owner = None;
        } else {
            return;
        }
        for vj in [
            prev.and_then(|p| self.viewer_index(p)),
            self.viewer_index(id),
        ]
        .into_iter()
        .flatten()
        {
            let owner = self.owner == Some(self.viewers[vj].id);
            self.plain(vj, wire::input_state(owner as u32, 0));
        }
    }
}
