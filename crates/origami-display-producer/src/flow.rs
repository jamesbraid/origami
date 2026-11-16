//! Sampling, sets, leases and the delivery rule.

use crate::engine::*;
use crate::lines::Lines;
use crate::storage;
use origami_display::v4::wire::{self, Damage, Frame};
use std::sync::atomic::{fence, Ordering};

impl Engine {
    fn quarter_ns(&self, h: u32) -> u64 {
        let mhz = self.heads[h as usize].spec.refresh_mhz;
        if mhz == 0 {
            4_000_000
        } else {
            1_000_000_000_000 / mhz as u64 / 4
        }
    }

    // --- state region ---

    fn head_base(h: u32) -> usize {
        wire::state::HEADER + h as usize * wire::state::HEAD_STRIDE
    }

    fn seqlock_write(&self, seq_off: usize, write: impl FnOnce(&crate::storage::Shm)) {
        let m = &self.state.map;
        let s = m.load32(seq_off, Ordering::Relaxed);
        m.store32(seq_off, s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        write(&self.state);
        m.store32(seq_off, s.wrapping_add(2), Ordering::Release);
    }

    pub(crate) fn publish_state(&self, h: u32) {
        use wire::state as s;
        let b = Self::head_base(h);
        let hd = &self.heads[h as usize];
        self.seqlock_write(b + s::FRAME_SEQ, |st| {
            let m = &st.map;
            m.store64(b + s::FRAME_SERIAL, hd.n, Ordering::Relaxed);
            m.store64(b + s::SAMPLE_NS, hd.sample_ns, Ordering::Relaxed);
            m.store64(b + s::PUBLISH_NS, hd.publish_ns, Ordering::Relaxed);
            m.store64(b + s::GUEST_NS, hd.guest_ns, Ordering::Relaxed);
            m.store32(b + s::SKIPPED, hd.skipped, Ordering::Relaxed);
        });
    }

    fn publish_cursor(&mut self, h: u32) {
        use wire::state as s;
        let b = Self::head_base(h);
        let c = &self.heads[h as usize].cursor;
        let (flags, x, y, img) = (c.flags, c.x, c.y, c.image_serial);
        self.seqlock_write(b + s::CURSOR_SEQ, |st| {
            let m = &st.map;
            m.store32(b + s::CURSOR_FLAGS, flags, Ordering::Relaxed);
            m.store32(b + s::CURSOR_X, x as u32, Ordering::Relaxed);
            m.store32(b + s::CURSOR_Y, y as u32, Ordering::Relaxed);
            m.store32(b + s::CURSOR_IMAGE_SERIAL, img, Ordering::Relaxed);
        });
        let seq = self.state.map.load32(b + s::CURSOR_SEQ, Ordering::Relaxed);
        self.heads[h as usize].cursor.seq = seq;
    }

    pub fn cursor_move(&mut self, h: u32, x: i32, y: i32, visible: bool) {
        let Some(hd) = self.heads.get_mut(h as usize) else {
            return;
        };
        hd.cursor.x = x;
        hd.cursor.y = y;
        hd.cursor.flags = visible as u32;
        self.publish_cursor(h);
        self.notify_cursor(h);
    }

    pub fn cursor_image(&mut self, h: u32, img: Option<wire::CursorImage>) {
        let Some(hd) = self.heads.get_mut(h as usize) else {
            return;
        };
        hd.cursor.image_serial = hd.cursor.image_serial.wrapping_add(1);
        let serial = hd.cursor.image_serial;
        let mut img = img.unwrap_or(wire::CursorImage {
            head: h,
            image_serial: serial,
            width: 0,
            height: 0,
            hot_x: 0,
            hot_y: 0,
            fourcc: wire::FOURCC_AR24,
            stride: 0,
            pixels: vec![],
        });
        img.head = h;
        img.image_serial = serial;
        hd.cursor.image = Some(img.clone());
        // The image goes out before the state region names its serial.
        for vi in 0..self.viewers.len() {
            if !self.viewers[vi].live {
                continue;
            }
            let v = &mut self.viewers[vi];
            let id_old = v
                .out
                .iter()
                .position(|m| m.tag == Tag::CursorImage(h) && m.sent == 0);
            if let Some(p) = id_old {
                let m = v.out.remove(p).unwrap();
                v.out_bytes -= m.bytes.len();
            }
            self.queue(vi, img.encode(), None, Tag::CursorImage(h));
        }
        self.publish_cursor(h);
        self.notify_cursor(h);
    }

    fn notify_cursor(&mut self, h: u32) {
        let seq = self.heads[h as usize].cursor.seq;
        for vi in 0..self.viewers.len() {
            if !self.viewers[vi].live {
                continue;
            }
            let Some(vh) = self.viewers[vi].heads.get_mut(h as usize) else {
                continue;
            };
            if vh.cursor_req.is_some_and(|c| c != seq) {
                vh.cursor_req = None;
                self.plain(vi, wire::cursor_notify(h, seq));
            }
        }
    }

    // --- leases ---

    /// Read the viewer's release table and end every lease it covers.
    pub(crate) fn refresh_leases(&mut self, vi: usize, h: u32) {
        let hu = h as usize;
        let credit = self.viewers[vi].credit as usize;
        let mut freed = vec![];
        {
            let v = &mut self.viewers[vi];
            let Some(fb) = v.feedback.as_ref() else {
                return;
            };
            let base = wire::feedback::HEADER + hu * wire::feedback::HEAD_STRIDE;
            for (i, lease) in v.heads[hu]
                .leases
                .iter_mut()
                .enumerate()
                .take(credit.max(1))
            {
                if let Some(l) = lease {
                    let rel = fb.map.load64(base + i * 8, Ordering::Acquire);
                    if rel >= l.seq {
                        freed.push((l.set, l.region));
                        *lease = None;
                    }
                }
            }
        }
        if !freed.is_empty() {
            for (set, region) in freed {
                self.unlend(h, set, region);
            }
            self.shrink_and_gc(h);
        }
    }

    fn unlend(&mut self, h: u32, set: u32, region: u32) {
        if let Some(s) = self.heads[h as usize].sets.iter_mut().find(|s| s.id == set) {
            if let Some(sl) = s.slots.iter_mut().find(|sl| sl.region == region) {
                sl.lent = sl.lent.saturating_sub(1);
            }
        }
    }

    pub(crate) fn free_credit(&mut self, vi: usize, h: u32) -> bool {
        self.refresh_leases(vi, h);
        let vh = &self.viewers[vi].heads[h as usize];
        vh.leases
            .iter()
            .take(vh.credit as usize)
            .any(|l| l.is_none())
            || vh
                .queued
                .as_ref()
                .is_some_and(|q| self.wholly_queued(vi, q.msg_id))
    }

    // --- sets ---

    fn slot_size(&self, h: u32) -> u64 {
        let hd = &self.heads[h as usize];
        align_up(hd.stride as u64 * hd.h as u64, wire::REGION_ALIGN).max(wire::REGION_ALIGN)
    }

    fn new_slot(&mut self, h: u32, writable: bool) -> Option<Slot> {
        let size = self.slot_size(h);
        if size > self.cfg.max_head_bytes {
            return None;
        }
        let shm = storage::create(size, writable).ok()?;
        let region = self.alloc_region();
        let hd = &self.heads[h as usize];
        Some(Slot {
            region,
            shm,
            stale: Lines::full(hd.h),
            lent: 0,
            retired: false,
        })
    }

    pub(crate) fn set_ix(&self, h: u32, id: u32) -> Option<usize> {
        self.heads[h as usize].sets.iter().position(|s| s.id == id)
    }

    /// Bring `set` up to its target size, announcing new slots to its viewers.
    fn grow_set(&mut self, h: u32, set_id: u32) -> bool {
        let mut grew = false;
        loop {
            let Some(si) = self.set_ix(h, set_id) else {
                return grew;
            };
            let (have, target, writable) = {
                let s = &self.heads[h as usize].sets[si];
                (
                    s.slots.iter().filter(|x| !x.retired).count(),
                    s.target,
                    s.writable,
                )
            };
            if have >= target {
                return grew;
            }
            let Some(slot) = self.new_slot(h, writable) else {
                return grew;
            };
            self.heads[h as usize].sets[si].slots.push(slot);
            grew = true;
            let ix = self.heads[h as usize].sets[si].slots.len() - 1;
            self.announce_to_set(h, si, ix);
        }
    }

    fn announce_to_set(&mut self, h: u32, si: usize, ix: usize) {
        let ids = self.heads[h as usize].sets[si].viewers.clone();
        for id in ids {
            if let Some(vi) = self.viewer_index(id) {
                let set = std::mem::replace(&mut self.heads[h as usize].sets[si], dummy_set());
                self.announce_slot(vi, &set, ix);
                self.heads[h as usize].sets[si] = set;
            }
        }
    }

    fn recompute_target(&mut self, h: u32, set_id: u32) {
        let Some(si) = self.set_ix(h, set_id) else {
            return;
        };
        let ids = self.heads[h as usize].sets[si].viewers.clone();
        let sum: u32 = ids
            .iter()
            .filter_map(|&id| self.viewer_index(id))
            .map(|vi| self.viewers[vi].heads[h as usize].credit)
            .sum();
        self.heads[h as usize].sets[si].target = ((1 + sum) as usize).clamp(2, wire::MAX_SLOTS);
    }

    /// Join `vi` to the right set for head `h` and tell it about the head.
    pub(crate) fn join_head(&mut self, vi: usize, h: u32) {
        let hu = h as usize;
        if self.viewers[vi].heads.len() <= hu {
            self.viewers[vi].heads.push(VHead::new());
        }
        let (width, height, present, unsupported) = {
            let hd = &self.heads[hu];
            (hd.w, hd.h, hd.present, hd.spec.unsupported)
        };
        let hello = self.viewers[vi].hello.clone().unwrap();
        let too_large = width > hello.max_width
            || height > hello.max_height
            || (hello.max_slot_bytes != 0
                && self.slot_size(h) > hello.max_slot_bytes as u64 * wire::REGION_ALIGN);
        {
            let credit = self.viewers[vi].credit;
            let vh = &mut self.viewers[vi].heads[hu];
            vh.too_large = too_large;
            vh.credit = credit;
            if vh.damage.len() != height {
                vh.damage = Lines::new(height);
            }
        }
        let want = present && width > 0 && !too_large && !unsupported;
        if want {
            self.enter_set(vi, h, false);
        }
        self.send_head(vi, h);
    }

    /// Put `vi` on a set of the right access for head `h`, creating one if needed.
    fn enter_set(&mut self, vi: usize, h: u32, exclusive: bool) {
        let hu = h as usize;
        let id = self.viewers[vi].id;
        let writable = self.viewers[vi].features & wire::feat::RW_PIXELS != 0;
        let found = self.heads[hu]
            .sets
            .iter()
            .find(|s| !s.dead && s.writable == writable && s.exclusive.is_none() && !exclusive)
            .map(|s| s.id);
        let set_id = match found {
            Some(s) => s,
            None => {
                let sid = self.next_set;
                self.next_set += 1;
                self.heads[hu].sets.push(Set {
                    id: sid,
                    writable,
                    exclusive: exclusive.then_some(id),
                    slots: vec![],
                    viewers: vec![],
                    target: 2,
                    dead: false,
                });
                sid
            }
        };
        let si = self.set_ix(h, set_id).unwrap();
        if !self.heads[hu].sets[si].viewers.contains(&id) {
            self.heads[hu].sets[si].viewers.push(id);
        }
        // A set holds at most 16 slots: grant less credit rather than starve the producer.
        let others: u32 = self.heads[hu].sets[si]
            .viewers
            .iter()
            .filter(|&&o| o != id)
            .filter_map(|&o| self.viewer_index(o))
            .map(|o| self.viewers[o].heads[hu].credit)
            .sum();
        let room = (wire::MAX_SLOTS as u32 - 1).saturating_sub(others).max(1);
        let vh = &mut self.viewers[vi].heads[hu];
        vh.credit = vh.credit.min(room);
        self.recompute_target(h, set_id);
        // A joiner's own slots are wholly stale new allocations; it learns all of them.
        let grew = self.grow_set(h, set_id);
        let si = self.set_ix(h, set_id).unwrap();
        for ix in 0..self.heads[hu].sets[si].slots.len() {
            let set = std::mem::replace(&mut self.heads[hu].sets[si], dummy_set());
            if !set.slots[ix].retired {
                self.announce_slot(vi, &set, ix);
            }
            self.heads[hu].sets[si] = set;
        }
        let vh = &mut self.viewers[vi].heads[hu];
        vh.set = Some(set_id);
        vh.damage_all = true;
        vh.reset_pending = true;
        if grew {
            self.broadcast_head_except(h, set_id, vi);
        }
    }

    fn broadcast_head_except(&mut self, h: u32, set_id: u32, skip: usize) {
        let si = self.set_ix(h, set_id).unwrap();
        for id in self.heads[h as usize].sets[si].viewers.clone() {
            if let Some(vj) = self.viewer_index(id) {
                if vj != skip {
                    self.send_head(vj, h);
                }
            }
        }
    }

    pub(crate) fn send_head(&mut self, vi: usize, h: u32) {
        let hu = h as usize;
        let hd = &self.heads[hu];
        let (w, hh, stride, present, unsupported) =
            (hd.w, hd.h, hd.stride, hd.present, hd.spec.unsupported);
        let (refresh, pm, label, src) = (
            hd.spec.refresh_mhz,
            hd.spec.pointer_mode,
            hd.spec.label.clone(),
            hd.spec.source_index,
        );
        let slots: Vec<u32> = match self.viewers[vi].heads[hu]
            .set
            .and_then(|s| self.set_ix(h, s))
        {
            Some(si) => hd_slots(&self.heads[hu].sets[si]),
            None => vec![],
        };
        let vh = &mut self.viewers[vi].heads[hu];
        if vh.announced_mode != (w, hh) {
            vh.announced_mode = (w, hh);
            vh.mode_serial = vh.mode_serial.wrapping_add(1);
        }
        let mut flags = 0;
        if present {
            flags |= wire::HEAD_PRESENT;
        }
        if vh.too_large {
            flags |= wire::HEAD_TOO_LARGE;
        }
        if unsupported {
            flags |= wire::HEAD_UNSUPPORTED;
        }
        let msg = wire::Head {
            head: h,
            flags,
            mode_serial: vh.mode_serial,
            source_index: src,
            width: w,
            height: hh,
            refresh_mhz: refresh,
            fourcc: self.heads[hu].spec.fourcc,
            stride,
            slot_region: slots,
            pointer_mode: pm,
            credit: vh.credit,
            label,
        };
        self.queue(vi, msg.encode(), None, Tag::Head(h));
    }

    pub(crate) fn leave_set(&mut self, vi: usize, h: u32) {
        let id = self.viewers[vi].id;
        let Some(set_id) = self.viewers[vi]
            .heads
            .get_mut(h as usize)
            .and_then(|vh| vh.set.take())
        else {
            return;
        };
        if let Some(si) = self.set_ix(h, set_id) {
            self.heads[h as usize].sets[si].viewers.retain(|&v| v != id);
        }
        self.recompute_target(h, set_id);
    }

    /// Remove unlent slots a smaller target no longer needs; free dead sets.
    pub(crate) fn shrink_and_gc(&mut self, h: u32) {
        let hu = h as usize;
        let mut gone = vec![];
        let mut shrunk = vec![];
        for set in &mut self.heads[hu].sets {
            if set.dead {
                continue;
            }
            set.slots.retain(|s| !(s.retired && s.lent == 0));
            let before = set.slots.len();
            while set.slots.iter().filter(|s| !s.retired).count() > set.target {
                let Some(p) = set.slots.iter().rposition(|s| !s.retired && s.lent == 0) else {
                    break;
                };
                gone.push(set.slots.remove(p).region);
            }
            if set.slots.len() != before {
                shrunk.push(set.id);
            }
        }
        for r in gone {
            self.region_gone(r);
        }
        for sid in shrunk {
            if let Some(si) = self.set_ix(h, sid) {
                for id in self.heads[hu].sets[si].viewers.clone() {
                    if let Some(vj) = self.viewer_index(id) {
                        self.send_head(vj, h);
                    }
                }
            }
        }
        self.gc_sets(h);
    }

    /// Free sets with no viewers and no leases.
    pub(crate) fn gc_sets(&mut self, h: u32) {
        let hu = h as usize;
        let mut freed = vec![];
        self.heads[hu].sets.retain(|s| {
            let dead_or_empty = s.dead || s.viewers.is_empty();
            if dead_or_empty && s.slots.iter().all(|sl| sl.lent == 0) {
                freed.extend(s.slots.iter().map(|sl| sl.region));
                false
            } else {
                true
            }
        });
        for r in freed {
            self.region_gone(r);
        }
    }

    /// Tell everyone who has `region` that nothing will name it again.
    pub(crate) fn region_gone(&mut self, region: u32) {
        for vi in 0..self.viewers.len() {
            if self.viewers[vi].announced.remove(&region) {
                self.plain(vi, wire::region_gone(region));
            }
        }
    }

    /// Stop writing a slot for good and give its set a fresh one. Unsent
    /// messages that name it are cancelled and undone.
    pub(crate) fn retire_slot(&mut self, h: u32, set_id: u32, region: u32) {
        let hu = h as usize;
        // Cancel unsent REGION, HEAD and FRAME messages that depend on it.
        for vi in 0..self.viewers.len() {
            if !self.viewers[vi].live {
                continue;
            }
            let v = &mut self.viewers[vi];
            let unsent_region = v
                .out
                .iter()
                .any(|m| m.tag == Tag::Region(region) && m.sent == 0);
            if unsent_region {
                let mut kept = std::collections::VecDeque::new();
                let mut dropped = 0;
                for m in v.out.drain(..) {
                    let drop_it =
                        m.sent == 0 && (m.tag == Tag::Region(region) || (m.tag == Tag::Head(h)));
                    if drop_it {
                        dropped += m.bytes.len();
                    } else {
                        kept.push_back(m);
                    }
                }
                v.out = kept;
                v.out_bytes -= dropped;
                v.announced.remove(&region);
            }
            let names = v
                .heads
                .get(hu)
                .and_then(|vh| vh.queued.as_ref())
                .is_some_and(|q| {
                    v.out.iter().any(|m| {
                        m.id == q.msg_id
                            && m.sent == 0
                            && matches!(&m.tag, Tag::Frame { region: r, .. } if *r == region)
                    })
                });
            if names {
                self.undo_queued(vi, h);
            }
        }
        let Some(si) = self.set_ix(h, set_id) else {
            return;
        };
        let set = &mut self.heads[hu].sets[si];
        if let Some(p) = set.slots.iter().position(|s| s.region == region) {
            let mut s = set.slots.remove(p);
            s.retired = true;
            // Held by a lease somewhere: keep the record so the lease still unlends it.
            if s.lent > 0 {
                set.slots.push(s);
            }
        }
        self.grow_set(h, set_id);
        self.region_gone(region);
        let si = self.set_ix(h, set_id).unwrap();
        for id in self.heads[hu].sets[si].viewers.clone() {
            if let Some(vj) = self.viewer_index(id) {
                self.send_head(vj, h);
            }
        }
        for vi in 0..self.viewers.len() {
            if self.viewers[vi].live
                && self.viewers[vi]
                    .heads
                    .get(hu)
                    .is_some_and(|vh| vh.queued.is_none() && vh.request.is_some())
            {
                self.deliver(vi, h);
            }
        }
    }

    /// Cancel the wholly queued `FRAME` of `vi` for `h`, as 7.3 undoes it.
    pub(crate) fn undo_queued(&mut self, vi: usize, h: u32) {
        let hu = h as usize;
        let Some(q) = self.viewers[vi].heads[hu].queued.take() else {
            return;
        };
        let v = &mut self.viewers[vi];
        let mut region = None;
        if let Some(p) = v.out.iter().position(|m| m.id == q.msg_id) {
            let m = v.out.remove(p).unwrap();
            v.out_bytes -= m.bytes.len();
            if let Tag::Frame { region: r, set, .. } = m.tag {
                region = Some((set, r));
            }
        }
        let vh = &mut v.heads[hu];
        vh.leases[q.lease] = None;
        vh.damage.union(&q.damage);
        vh.damage_all |= q.damage_all;
        vh.last_lent = q.prev_last_lent;
        if vh.request.is_none() {
            vh.request = Some(Rq {
                bound: None,
                ..q.req
            });
        }
        if let Some((set, r)) = region {
            self.unlend(h, set, r);
        }
    }

    /// The viewer is leaving head `h`: retire the slots it held that were sent.
    pub(crate) fn depart_head(&mut self, vi: usize, h: u32) {
        let hu = h as usize;
        if self.viewers[vi].heads.len() <= hu {
            return;
        }
        let queued_msg = self.viewers[vi].heads[hu].queued.as_ref().map(|q| q.msg_id);
        let whole = queued_msg.is_some_and(|m| self.wholly_queued(vi, m));
        let leases: Vec<(usize, u32, u32)> = self.viewers[vi].heads[hu]
            .leases
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.as_ref().map(|l| (i, l.set, l.region)))
            .collect();
        self.leave_set(vi, h);
        for (i, set, region) in leases {
            self.viewers[vi].heads[hu].leases[i] = None;
            self.unlend(h, set, region);
            let is_queued = whole
                && self.viewers[vi].heads[hu]
                    .queued
                    .as_ref()
                    .is_some_and(|q| q.lease == i);
            if !is_queued {
                self.retire_slot(h, set, region);
            }
        }
        self.shrink_and_gc(h);
    }

    // --- requests ---

    pub(crate) fn on_request(&mut self, vi: usize, r: wire::Request) {
        let h = r.head;
        let hu = h as usize;
        if hu >= self.heads.len() || !self.heads[hu].present || self.viewers[vi].heads.len() <= hu {
            return;
        }
        if r.kinds & wire::REQ_FRAME != 0 {
            let old = self.viewers[vi].heads[hu].request;
            let sample_now = r.flags & wire::REQ_SAMPLE_NOW != 0;
            let (bound, pending) = match old {
                Some(o) if !sample_now => (o.bound, o.sample_now_pending),
                _ => (None, false),
            };
            self.viewers[vi].heads[hu].request = Some(Rq {
                frame_seen: r.frame_seen,
                bound,
                sample_now_pending: pending || sample_now,
            });
            self.refresh_leases(vi, h);
            self.bind_if_credit(vi, h);
            let unbound_flag = self.heads[hu].marks.pending()
                && self.viewers[vi].heads[hu]
                    .request
                    .is_some_and(|q| q.bound.is_none())
                && self.free_credit(vi, h);
            if unbound_flag && !self.sampling_in_progress(h) {
                self.schedule_sample(h);
            }
        }
        if r.kinds & wire::REQ_CURSOR != 0 {
            let seq = self.heads[hu].cursor.seq;
            if seq != r.cursor_seen {
                self.plain(vi, wire::cursor_notify(h, seq));
            } else {
                self.viewers[vi].heads[hu].cursor_req = Some(r.cursor_seen);
            }
        }
        self.evaluate(h);
    }

    fn sampling_in_progress(&self, h: u32) -> bool {
        let hd = &self.heads[h as usize];
        hd.held.is_some() || hd.push_open.is_some() || hd.reserved.is_some()
    }

    /// Bind a `SAMPLE_NOW` once its viewer has credit to receive the result.
    fn bind_if_credit(&mut self, vi: usize, h: u32) {
        let hu = h as usize;
        let Some(rq) = self.viewers[vi].heads[hu].request else {
            return;
        };
        if !rq.sample_now_pending || !self.free_credit(vi, h) {
            return;
        }
        let id = self.bind_sample(h);
        if let Some(q) = self.viewers[vi].heads[hu].request.as_mut() {
            q.bound = Some(id);
            q.sample_now_pending = false;
        }
    }

    // --- sampling ---

    fn bind_sample(&mut self, h: u32) -> u64 {
        let q = self.quarter_ns(h);
        let now = now_ns();
        let hd = &self.heads[h as usize];
        // A held sample (the test hook) or a composition asked for but not begun
        // starts after this request, so it can be shared whatever its age.
        if let Some((id, ..)) = hd.held {
            return id;
        }
        if let Some(id) = hd.reserved {
            return id;
        }
        match hd.latest {
            Some((id, start)) if now.saturating_sub(start) <= q => id,
            _ => self.schedule_sample(h),
        }
    }

    /// Start a sample no request is bound to. Returns its id.
    pub(crate) fn schedule_sample(&mut self, h: u32) -> u64 {
        let hu = h as usize;
        if self.heads[hu].spec.pull {
            let hd = &mut self.heads[hu];
            hd.sample_ctr += 1;
            let id = hd.sample_ctr;
            hd.latest = Some((id, now_ns()));
            self.execute_sample(h, id);
            id
        } else {
            let hd = &mut self.heads[hu];
            if let Some(id) = hd.reserved {
                return id;
            }
            hd.sample_ctr += 1;
            let id = hd.sample_ctr;
            hd.reserved = Some(id);
            hd.latest = Some((id, now_ns()));
            if !self.paused {
                self.host.request_frame(h);
            } else {
                self.complete_bound_as_unchanged(h);
            }
            id
        }
    }

    fn execute_sample(&mut self, h: u32, id: u64) {
        let hd = &mut self.heads[h as usize];
        let start = now_ns();
        if hd.hold_next {
            hd.hold_next = false;
            hd.held = Some((id, start, hd.mode_gen));
            return;
        }
        self.run_sample(h, id, start);
    }

    pub(crate) fn run_sample(&mut self, h: u32, id: u64, start: u64) {
        let hu = h as usize;
        self.host.acquire(h);
        self.stats.samples += 1;
        let Engine {
            host, heads, stats, ..
        } = self;
        let hd = &mut heads[hu];
        let taken = hd.marks.take(hd.h);
        let mut changed = Lines::new(hd.h);
        let (w, stride) = (hd.w as usize, hd.stride as usize);
        let mut row = vec![0u32; w];
        for y in taken.iter() {
            host.resolve_line(h, y, &mut row);
            stats.lines_produced += 1;
            // SAFETY: a u32 slice viewed as bytes; the host is little endian (checked at start).
            let bytes = unsafe { std::slice::from_raw_parts(row.as_ptr().cast::<u8>(), w * 4) };
            let dst = &mut hd.reference[y as usize * stride..y as usize * stride + w * 4];
            if dst != bytes {
                dst.copy_from_slice(bytes);
                changed.set(y);
                stats.lines_stored += 1;
            }
        }
        self.complete_sample(h, id, start, changed);
    }

    /// Steps 4 and 5 of a sample: raise `N`, spread the damage, deliver.
    pub(crate) fn complete_sample(&mut self, h: u32, id: u64, sample_ns: u64, mut changed: Lines) {
        let hu = h as usize;
        let first = !self.heads[hu].mode_ready;
        if first {
            self.heads[hu].mode_ready = true;
            changed.set_all();
        }
        if !changed.is_empty() {
            let hd = &mut self.heads[hu];
            hd.n += 1;
            hd.sample_ns = sample_ns;
            hd.publish_ns = now_ns();
            hd.guest_ns = 0;
            for set in hd.sets.iter_mut().filter(|s| !s.dead) {
                for sl in set.slots.iter_mut() {
                    sl.stale.union(&changed);
                }
            }
            self.stats.publishes += 1;
            for v in self.viewers.iter_mut().filter(|v| v.live) {
                if let Some(vh) = v.heads.get_mut(hu) {
                    if vh.damage.len() != changed.len() {
                        vh.damage = Lines::new(changed.len());
                    }
                    vh.damage.union(&changed);
                }
            }
            self.publish_state(h);
        }
        let hd = &mut self.heads[hu];
        hd.done_ctr = hd.done_ctr.max(id);
        self.evaluate(h);
    }

    /// Every bound sample completes as "no change" when its source stops.
    pub(crate) fn complete_bound_as_unchanged(&mut self, h: u32) {
        let hd = &mut self.heads[h as usize];
        hd.done_ctr = hd.sample_ctr;
        hd.reserved = None;
        self.evaluate(h);
    }

    /// Run samples the marks call for, if a viewer is waiting with credit.
    pub fn bottom_half(&mut self) {
        for h in 0..self.heads.len() as u32 {
            let hu = h as usize;
            if !self.heads[hu].present
                || !self.heads[hu].marks.pending()
                || self.sampling_in_progress(h)
            {
                continue;
            }
            if self.heads[hu].w == 0 {
                continue;
            }
            let mut wanted = false;
            for vi in 0..self.viewers.len() {
                if self.viewers[vi].live
                    && self.viewers[vi]
                        .heads
                        .get(hu)
                        .is_some_and(|vh| vh.request.is_some())
                    && self.free_credit(vi, h)
                {
                    wanted = true;
                    break;
                }
            }
            if wanted {
                self.stats.wakeups += 1;
                self.schedule_sample(h);
            }
        }
        for h in 0..self.heads.len() as u32 {
            self.evaluate(h);
        }
    }

    pub fn evaluate(&mut self, h: u32) {
        for vi in 0..self.viewers.len() {
            if self.viewers[vi].live
                && self.viewers[vi]
                    .heads
                    .get(h as usize)
                    .is_some_and(|vh| vh.request.is_some())
            {
                self.deliver(vi, h);
            }
        }
    }

    // --- push hosts ---

    /// A push host starts composing `h`: returns the sample id and its start time.
    pub fn push_begin(&mut self, h: u32) -> Option<(u64, u64)> {
        let hu = h as usize;
        let hd = self.heads.get_mut(hu)?;
        let now = now_ns();
        let id = hd.reserved.take().unwrap_or_else(|| {
            hd.sample_ctr += 1;
            hd.sample_ctr
        });
        hd.push_open = Some((id, now));
        hd.latest = Some((id, now));
        // Changes marked before this point are in the picture the host composes next.
        let _ = hd.marks.take(hd.h);
        Some((id, now))
    }

    /// The host finished composing: `pixels` is the whole picture at the head's
    /// stride (or `None`: nothing changed). Compares it into the reference frame.
    pub fn push_complete(&mut self, h: u32, id: u64, pixels: Option<&[u8]>) {
        let stride = self
            .heads
            .get(h as usize)
            .map_or(0, |hd| hd.stride as usize);
        self.push_complete_strided(h, id, pixels, stride);
    }

    /// As [`push_complete`](Self::push_complete) for rows `src_stride` bytes
    /// apart, so a compositor's buffer is compared where it is.
    pub fn push_complete_strided(
        &mut self,
        h: u32,
        id: u64,
        pixels: Option<&[u8]>,
        src_stride: usize,
    ) {
        let Some(hd) = self.heads.get_mut(h as usize) else {
            return;
        };
        let start = hd.push_open.take().map_or_else(now_ns, |(_, s)| s);
        // A picture from before a mode change no longer fits the head: ignore it.
        let pixels = pixels.filter(|p| fits(p, src_stride, hd.w as usize * 4, hd.h as usize));
        if hd.hold_next {
            // The test hook: this composition completes at CONTROL op 2.
            hd.hold_next = false;
            hd.held = Some((id, start, hd.mode_gen));
            hd.held_pic = pixels.map(|p| {
                rows_at(
                    p,
                    src_stride,
                    hd.stride as usize,
                    hd.w as usize * 4,
                    hd.h as usize,
                )
            });
            return;
        }
        self.push_apply(h, id, start, pixels, src_stride);
    }

    pub(crate) fn push_apply(
        &mut self,
        h: u32,
        id: u64,
        start: u64,
        pixels: Option<&[u8]>,
        src_stride: usize,
    ) {
        let hu = h as usize;
        let hd = &mut self.heads[hu];
        let mut changed = Lines::new(hd.h);
        let (stride, w) = (hd.stride as usize, hd.w as usize * 4);
        if let Some(p) = pixels.filter(|p| fits(p, src_stride, w, hd.h as usize)) {
            self.stats.samples += 1;
            for y in 0..hd.h as usize {
                let (a, b) = (
                    &p[y * src_stride..y * src_stride + w],
                    &mut hd.reference[y * stride..y * stride + w],
                );
                self.stats.lines_produced += 1;
                if a != b {
                    b.copy_from_slice(a);
                    changed.set(y as u32);
                    self.stats.lines_stored += 1;
                }
            }
        }
        self.complete_sample(h, id, start, changed);
        if self.heads[hu].reserved.is_some() && !self.paused {
            self.host.request_frame(h);
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if paused {
            for h in 0..self.heads.len() as u32 {
                self.complete_bound_as_unchanged(h);
            }
        }
    }

    // --- modes ---

    pub fn set_mode(&mut self, h: u32, w: u32, ht: u32) {
        let hu = h as usize;
        let Some(hd) = self.heads.get_mut(hu).filter(|x| x.present) else {
            return;
        };
        hd.w = w;
        hd.h = ht;
        hd.stride = align_up(w as u64 * 4, wire::STRIDE_ALIGN as u64) as u32;
        hd.reference = vec![0; hd.stride as usize * ht as usize];
        hd.mode_gen += 1;
        hd.mode_ready = false;
        hd.held = None;
        hd.push_open = None;
        hd.marks.mark_all();
        for s in &mut hd.sets {
            s.dead = true;
        }
        // Bound samples complete; the new mode must be sampled before anyone is due.
        self.complete_bound_as_unchanged(h);
        for vi in 0..self.viewers.len() {
            if !self.viewers[vi].live || self.viewers[vi].heads.len() <= hu {
                continue;
            }
            let vh = &mut self.viewers[vi].heads[hu];
            vh.set = None;
            vh.damage = Lines::new(ht);
            vh.queued = None;
            // A frame still wholly queued belongs to the old mode and is withdrawn.
            let v = &mut self.viewers[vi];
            let mut kept = std::collections::VecDeque::new();
            let mut undo = vec![];
            for m in v.out.drain(..) {
                if m.sent == 0 && matches!(&m.tag, Tag::Frame { head, .. } if *head == h) {
                    v.out_bytes -= m.bytes.len();
                    if let Tag::Frame { set, region, .. } = m.tag {
                        undo.push((set, region));
                    }
                } else {
                    kept.push_back(m);
                }
            }
            v.out = kept;
            for l in v.heads[hu].leases.iter_mut() {
                if let Some(x) = l {
                    if undo.contains(&(x.set, x.region)) {
                        *l = None;
                    }
                }
            }
            for (set, region) in undo {
                self.unlend(h, set, region);
            }
            self.join_head(vi, h);
        }
        self.gc_sets(h);
        self.evaluate(h);
    }

    // --- delivery ---

    pub(crate) fn deliver(&mut self, vi: usize, h: u32) {
        let hu = h as usize;
        {
            let hd = &self.heads[hu];
            if !hd.present || hd.w == 0 || !hd.mode_ready || hd.n == 0 {
                return;
            }
        }
        self.bind_if_credit(vi, h);
        self.refresh_leases(vi, h);
        let (n, done) = (self.heads[hu].n, self.heads[hu].done_ctr);
        let vh = &self.viewers[vi].heads[hu];
        let Some(rq) = vh.request else { return };
        if rq.bound.is_some_and(|b| done < b) || rq.sample_now_pending {
            return;
        }
        let (seen, last) = if vh.reset_pending {
            (0, 0)
        } else {
            (rq.frame_seen, vh.last_lent)
        };
        if n <= seen.max(last) {
            return;
        }
        let Some(set_id) = vh.set else { return };
        // The lease index: a wholly queued frame's, else the lowest free one.
        let replacing = vh
            .queued
            .as_ref()
            .filter(|q| self.wholly_queued(vi, q.msg_id))
            .map(|q| q.lease);
        let credit = vh.credit as usize;
        let Some(lease_ix) =
            replacing.or_else(|| vh.leases.iter().take(credit).position(|l| l.is_none()))
        else {
            return;
        };
        let (mut damage, mut damage_all, prev_last, old) = (
            vh.damage.clone(),
            vh.damage_all,
            vh.last_lent,
            replacing.is_some(),
        );
        let mut saved_prev = prev_last;
        if old {
            let q = self.viewers[vi].heads[hu].queued.take().unwrap();
            if let Some(p) = self.viewers[vi].out.iter().position(|m| m.id == q.msg_id) {
                let m = self.viewers[vi].out.remove(p).unwrap();
                self.viewers[vi].out_bytes -= m.bytes.len();
                if let Tag::Frame { set, region, .. } = m.tag {
                    self.unlend(h, set, region);
                }
            }
            self.viewers[vi].heads[hu].leases[lease_ix] = None;
            damage.union(&q.damage);
            damage_all |= q.damage_all;
            saved_prev = q.prev_last_lent;
        }
        let Some(si) = self.set_ix(h, set_id) else {
            return;
        };
        let Some(ix) = self.pick_slot(h, si) else {
            self.heads[hu].skipped += 1;
            self.publish_state(h);
            // Put back what a replaced queued frame held, so the request is not lost.
            let vh = &mut self.viewers[vi].heads[hu];
            vh.damage.union(&damage);
            vh.damage_all |= damage_all;
            vh.last_lent = saved_prev;
            return;
        };
        if !self.heads[hu].sets[si].slots[ix].stale.is_empty() {
            self.fill_slot(h, si, ix);
        }
        let hd = &mut self.heads[hu];
        let region = hd.sets[si].slots[ix].region;
        hd.sets[si].slots[ix].lent += 1;
        let (serial, sample_ns, guest_ns) = (hd.n, hd.sample_ns, hd.guest_ns);
        let v = &mut self.viewers[vi];
        let vh = &mut v.heads[hu];
        let lease_seq = vh.next_lease_seq;
        vh.next_lease_seq += 1;
        let dmg = if damage_all {
            Damage::Whole
        } else {
            let runs = damage.runs();
            if runs.len() <= wire::MAX_RECTS {
                let wpx = self.heads[hu].w;
                Damage::Rects(runs.iter().map(|&(y, c)| [0, y, wpx, c]).collect())
            } else {
                Damage::Lines(damage.words().to_vec())
            }
        };
        let frame = Frame {
            head: h,
            region,
            serial,
            mode_serial: vh.mode_serial,
            sample_ns,
            publish_ns: now_ns(),
            guest_ns,
            lease: lease_ix as u16,
            frame_flags: 0,
            lease_seq,
            damage: dmg,
        };
        vh.leases[lease_ix] = Some(Lease {
            seq: lease_seq,
            region,
            set: set_id,
        });
        let req = vh.request.take().unwrap();
        vh.last_lent = serial;
        vh.damage = Lines::new(damage.len());
        vh.damage_all = false;
        let msg_id = self.queue(
            vi,
            frame.encode(),
            None,
            Tag::Frame {
                head: h,
                set: set_id,
                region,
            },
        );
        let q = Queued {
            msg_id,
            lease: lease_ix,
            prev_last_lent: saved_prev,
            damage,
            damage_all,
            req,
        };
        self.viewers[vi].heads[hu].queued = Some(q);
        let unsent = self.viewers[vi]
            .out
            .iter()
            .filter(|m| m.sent == 0 && matches!(&m.tag, Tag::Frame { head, .. } if *head == h))
            .count() as u32;
        self.viewers[vi].max_queued = self.viewers[vi].max_queued.max(unsent);
    }

    fn pick_slot(&mut self, h: u32, si: usize) -> Option<usize> {
        let set = &self.heads[h as usize].sets[si];
        if let Some(p) = set
            .slots
            .iter()
            .position(|s| !s.retired && s.stale.is_empty())
        {
            return Some(p);
        }
        set.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.retired && s.lent == 0)
            .min_by_key(|(_, s)| s.stale.count())
            .map(|(i, _)| i)
    }

    fn fill_slot(&mut self, h: u32, si: usize, ix: usize) {
        let Engine { heads, stats, .. } = self;
        let hd = &mut heads[h as usize];
        let stride = hd.stride as usize;
        let slot = &mut hd.sets[si].slots[ix];
        for (y0, count) in slot.stale.runs() {
            let (off, len) = (y0 as usize * stride, count as usize * stride);
            slot.shm.map.copy_in(off, &hd.reference[off..off + len]);
            stats.bytes_copied += len as u64;
        }
        stats.lines_copied += slot.stale.count() as u64;
        stats.fills += 1;
        slot.stale.clear();
    }

    // --- re-homing ---

    pub(crate) fn rehome(&mut self, vi: usize, h: u32, exclusive: bool) {
        self.leave_set(vi, h);
        let hu = h as usize;
        let hd = &self.heads[hu];
        if hd.present && hd.w > 0 && !self.viewers[vi].heads[hu].too_large && !hd.spec.unsupported {
            self.enter_set(vi, h, exclusive);
        }
        self.send_head(vi, h);
        self.shrink_and_gc(h);
        self.evaluate(h);
    }

    pub(crate) fn on_import_failed(&mut self, vi: usize, region: u32, reason: u32) {
        // Which head's set holds the region?
        let found = (0..self.heads.len() as u32).find(|&h| {
            self.heads[h as usize]
                .sets
                .iter()
                .any(|s| s.slots.iter().any(|sl| sl.region == region))
        });
        let Some(h) = found else { return };
        match reason {
            3 => {
                if self.viewers[vi].dev_lost_used {
                    self.fail(vi, wire::err::UNSUPPORTED, "no storage class left");
                } else {
                    self.viewers[vi].dev_lost_used = true;
                    self.rehome(vi, h, true);
                }
            }
            5 => {
                self.viewers[vi].heads[h as usize].too_large = true;
                self.leave_set(vi, h);
                self.send_head(vi, h);
                self.shrink_and_gc(h);
            }
            6 if self.viewers[vi].features & wire::feat::RW_PIXELS != 0 => {
                self.rehome(vi, h, false)
            }
            7 => self.rehome(vi, h, false),
            _ => self.fail(vi, wire::err::UNSUPPORTED, "no storage class left"),
        }
    }

    pub(crate) fn on_set_storage(&mut self, vi: usize, head: u32, storage: [u32; 8]) {
        let list: Vec<u32> = storage.iter().copied().take_while(|&s| s != 0).collect();
        if !list.contains(&wire::STORAGE_SHM) {
            return self.fail(vi, wire::err::PROTOCOL, "storage list lacks shm");
        }
        self.viewers[vi].storage = list;
        let heads: Vec<u32> = if head == u32::MAX {
            (0..self.heads.len() as u32).collect()
        } else {
            vec![head]
        };
        for h in heads {
            if (h as usize) < self.heads.len() && self.viewers[vi].heads.len() > h as usize {
                self.viewers[vi].heads[h as usize].failed.clear();
                self.rehome(vi, h, false);
            }
        }
    }
}

fn hd_slots(s: &Set) -> Vec<u32> {
    s.slots
        .iter()
        .filter(|x| !x.retired)
        .map(|x| x.region)
        .take(wire::MAX_SLOTS)
        .collect()
}

/// Whether `h` rows of `w` bytes, `stride` apart, lie within `p`.
fn fits(p: &[u8], stride: usize, w: usize, h: usize) -> bool {
    h == 0 || (stride >= w && p.len() >= stride * (h - 1) + w)
}

/// The first `w` bytes of `h` rows of `p`, `from` apart, laid out `to` apart.
fn rows_at(p: &[u8], from: usize, to: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0; to * h];
    for y in 0..h {
        out[y * to..y * to + w].copy_from_slice(&p[y * from..y * from + w]);
    }
    out
}

pub(crate) fn dummy_set() -> Set {
    Set {
        id: 0,
        writable: false,
        exclusive: None,
        slots: vec![],
        viewers: vec![],
        target: 0,
        dead: true,
    }
}
