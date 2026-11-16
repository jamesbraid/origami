//! The mandatory checks of the conformance suite, one function each.

use crate::client::*;
use crate::ensure;
use origami_display::v4::session::FrameLease;
use origami_display::v4::testext::{self, echo, kind, op, pattern};
use origami_display::v4::wire::{self, Damage, ToViewer};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

pub struct Ctx {
    pub t: Target,
    pub w: u32,
    pub h: u32,
}

pub type Check = fn(&Ctx) -> R<()>;

const SEED: u64 = 0xC0FFEE;
const WAIT: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(250);

pub const CHECKS: &[(&str, Check)] = &[
    ("framing/fragmented-and-coalesced", framing_split),
    ("framing/unknown-optional-skipped", framing_unknown),
    ("framing/unknown-must-refused", framing_must),
    ("framing/short-message", framing_short),
    ("framing/major-5", framing_major),
    ("framing/reserved-ignored", framing_reserved),
    ("framing/tail-overrun", framing_tail),
    ("handles/order-and-trailing-field", handles_order),
    ("handles/window-of-32", handles_window),
    ("handles/viewer-handle-refused", handles_viewer),
    ("handles/bad-handles-seen", handles_bad_seen),
    ("regions/sizes-seals-mappings", regions_seals),
    ("lent/no-write-while-lent", lent_no_write),
    ("credit/ceiling", credit_ceiling),
    ("flow/last-frame", flow_last_frame),
    ("flow/pending-changes", flow_pending),
    ("flow/still-head-on-connect", flow_still_head),
    ("flow/rehome-keeps-the-lease", flow_rehome_lease),
    ("flow/request-racing-a-mark", flow_race_mark),
    ("flow/unchanged-sample-newer-frame", flow_unchanged_newer),
    ("flow/no-unchanged-publish", flow_no_unchanged),
    ("flow/retries-crossing-delivery", flow_retries),
    ("sample-now/step-visible", sn_basic),
    ("sample-now/bound-to-its-sample", sn_bound),
    ("pixels/damage-coverage", pix_damage),
    ("pixels/completeness-two-viewers", pix_complete),
    ("pixels/two-sets-alternating", pix_two_sets),
    ("pixels/publication-versus-fill", pix_concurrent),
    ("sets/slot-list-changes", sets_slot_list),
    (
        "sets/slot-list-head-resets-nothing",
        sets_head_resets_nothing,
    ),
    ("sets/rehoming", sets_rehoming),
    ("heads/mode-and-heads", heads_mode),
    ("heads/disconnect-retires", heads_disconnect),
    ("heads/cancellation", heads_cancel),
    ("heads/cancellation-after-rehome", heads_cancel_rehome),
    ("cursor/records-and-notify", cursor_all),
    ("state/frame-record", state_record),
    ("time/timestamps", time_stamps),
    ("input/reports", input_reports),
    ("input/ownership", input_owner),
    ("limits/queue-overflow", limits_overflow),
    ("limits/head-too-large", limits_too_large),
    ("security/silent-connection", security_silent),
];

// --- pictures ---

fn picture(f: &FrameLease) -> Vec<u32> {
    let (w, h, stride) = (f.width as usize, f.height as usize, f.stride as usize);
    let px = f.pixels();
    let mut v = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let o = y * stride + x * 4;
            v.push(u32::from_le_bytes(px[o..o + 4].try_into().unwrap()) & 0xFF_FFFF);
        }
    }
    v
}

fn step_of(pic: &[u32]) -> u32 {
    (0..8).map(|i| (pic[i] & 0xF) << (4 * i)).sum()
}

/// Check the frame equals the picture of the step in its first line; return the step.
fn verify(f: &FrameLease, pat: u32, seed: u64) -> R<(u32, Vec<u32>)> {
    let pic = picture(f);
    let step = step_of(&pic);
    let want = testext::pattern_picture(pat, seed, step, f.width, f.height);
    if let Some(i) = (0..pic.len()).find(|&i| pic[i] != want[i]) {
        let (x, y) = (i as u32 % f.width, i as u32 / f.width);
        return Err(format!(
            "serial {} step {step}: pixel ({x},{y}) is {:06x}, expected {:06x}",
            f.frame.serial, pic[i], want[i]
        ));
    }
    Ok((step, pic))
}

fn check_damage(prev: &[u32], cur: &[u32], f: &FrameLease) -> R<()> {
    let w = f.width as usize;
    for y in 0..f.height {
        if !f.frame.damage.covers_line(y, f.height) {
            let r = y as usize * w..(y as usize + 1) * w;
            ensure!(
                prev[r.clone()] == cur[r],
                "serial {}: line {y} changed outside the damage",
                f.frame.serial
            );
        }
    }
    Ok(())
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// --- framing ---

fn framing_split(c: &Ctx) -> R<()> {
    let mut r = Raw::connect(&c.t)?;
    for b in Raw::hello(1).encode() {
        r.send(&[b])?;
    }
    let end = Instant::now() + WAIT;
    while r.count(|m| matches!(m, ToViewer::Ready)) == 0 {
        ensure!(
            Instant::now() < end,
            "no READY after a one-byte-at-a-time HELLO"
        );
        r.read(ms(50))?;
    }
    let req = wire::Request {
        head: 0,
        kinds: wire::REQ_FRAME,
        frame_seen: 0,
        cursor_seen: 0,
        flags: 0,
    }
    .encode();
    let mut two = req.clone();
    two.extend_from_slice(&req);
    r.send(&two)?;
    let end = Instant::now() + WAIT;
    while r.count(|m| matches!(m, ToViewer::Frame(_))) == 0 {
        ensure!(Instant::now() < end, "no FRAME for two coalesced requests");
        r.read(ms(50))?;
    }
    Ok(())
}

fn raw_ready(c: &Ctx) -> R<Raw> {
    let mut r = Raw::connect(&c.t)?;
    r.handshake(1)?;
    Ok(r)
}

fn plain_request() -> Vec<u8> {
    wire::Request {
        head: 0,
        kinds: wire::REQ_FRAME,
        frame_seen: 0,
        cursor_seen: 0,
        flags: 0,
    }
    .encode()
}

fn framing_unknown(c: &Ctx) -> R<()> {
    let mut r = raw_ready(c)?;
    for ty in [0x0050u16, 0x7F01] {
        let mut m = wire::Out::new(ty, 0);
        m.u64(0).u64(0);
        r.send(&m.finish())?;
    }
    r.send(&plain_request())?;
    r.read(ms(1000))?;
    ensure!(
        r.count(|m| matches!(m, ToViewer::Frame(_))) > 0,
        "no FRAME after unknown optional types"
    );
    ensure!(
        r.count(|m| matches!(m, ToViewer::Error(_))) == 0,
        "unknown optional type caused an ERROR"
    );
    Ok(())
}

fn framing_must(c: &Ctx) -> R<()> {
    let mut r = raw_ready(c)?;
    let mut m = wire::Out::new(0x0050, wire::F_MUST);
    m.u64(0);
    r.send(&m.finish())?;
    let e = r.error(WAIT)?;
    ensure!(
        e.code == wire::err::UNSUPPORTED,
        "expected UNSUPPORTED, got code {}",
        e.code
    );
    Ok(())
}

fn framing_short(c: &Ctx) -> R<()> {
    let mut r = raw_ready(c)?;
    let mut m = plain_request();
    m.truncate(16);
    m[4..8].copy_from_slice(&16u32.to_le_bytes());
    r.send(&m)?;
    let e = r.error(WAIT)?;
    ensure!(
        e.code == wire::err::PROTOCOL,
        "expected PROTOCOL, got code {}",
        e.code
    );
    Ok(())
}

fn framing_major(c: &Ctx) -> R<()> {
    let mut r = Raw::connect(&c.t)?;
    let mut h = Raw::hello(1);
    h.major = 5;
    r.send(&h.encode())?;
    let e = r.error(WAIT)?;
    ensure!(
        e.code == wire::err::VERSION,
        "expected VERSION, got code {}",
        e.code
    );
    ensure!(
        e.major_min <= 4 && 4 <= e.major_max,
        "range {}..{} does not include 4",
        e.major_min,
        e.major_max
    );
    Ok(())
}

fn framing_reserved(c: &Ctx) -> R<()> {
    let mut r = raw_ready(c)?;
    let mut k = wire::key(0, 30, 1);
    k[20..24].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    r.send(&k)?;
    r.send(&wire::key(0, 30, 0))?;
    r.send(&plain_request())?;
    r.read(ms(1000))?;
    ensure!(
        r.count(|m| matches!(m, ToViewer::Error(_))) == 0,
        "reserved field caused an ERROR"
    );
    ensure!(
        r.count(|m| matches!(m, ToViewer::Frame(_))) > 0,
        "no FRAME after reserved fields"
    );
    Ok(())
}

fn framing_tail(c: &Ctx) -> R<()> {
    let mut r = Raw::connect(&c.t)?;
    let mut m = Raw::hello(1).encode();
    m[100..104].copy_from_slice(&1000u32.to_le_bytes());
    r.send(&m)?;
    let e = r.error(WAIT)?;
    ensure!(
        e.code == wire::err::PROTOCOL,
        "expected PROTOCOL, got code {}",
        e.code
    );
    Ok(())
}

// --- handles ---

fn handles_order(c: &Ctx) -> R<()> {
    let r = raw_ready(c)?;
    let mut n = 0;
    for (m, _, raw, fd) in &r.received {
        if let ToViewer::Region(reg) = m {
            n += 1;
            let fd = fd.as_ref().ok_or("REGION without a descriptor")?;
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            ensure!(
                unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } == 0,
                "fstat failed"
            );
            ensure!(
                st.st_size as u64 == reg.size,
                "region {} descriptor is {} bytes, message says {}",
                reg.id,
                st.st_size,
                reg.size
            );
            let tail = u64::from_le_bytes(raw[raw.len() - 8..].try_into().unwrap());
            ensure!(
                tail == 0 && raw.len() >= 16,
                "trailing handle field is {tail} on POSIX"
            );
        }
    }
    ensure!(n >= 3, "expected state, feedback and slot regions, saw {n}");
    ensure!(
        r.reader.handles_parsed == n,
        "{} HANDLE messages parsed, {n} regions",
        r.reader.handles_parsed
    );
    Ok(())
}

fn handles_window(c: &Ctx) -> R<()> {
    let mut r = Raw::connect(&c.t)?;
    let mut hello = Raw::hello(4);
    hello.features |= wire::feat::RW_PIXELS;
    r.send(&hello.encode())?;
    r.read(ms(500))?;
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    let mut added = vec![];
    for _ in 0..9 {
        let before = r.reader.handles_parsed;
        let Ok(h) = add_head(&mut ctl) else { break };
        r.read(ms(100))?;
        added.push(h);
        if r.reader.handles_parsed == before && r.reader.handles_parsed >= 32 {
            break;
        }
    }
    r.read(ms(300))?;
    let parsed = r.reader.handles_parsed;
    ensure!(
        parsed <= wire::HANDLE_WINDOW,
        "{parsed} HANDLE messages in flight with none acknowledged"
    );
    let result = if parsed < wire::HANDLE_WINDOW {
        Err(format!(
            "the window never filled ({parsed} handles); the producer has too few heads to test it"
        ))
    } else {
        r.send(&wire::handles_seen(parsed))?;
        r.read(ms(500))?;
        ensure!(
            r.reader.handles_parsed > parsed,
            "no further handles after HANDLES_SEEN"
        );
        Ok(())
    };
    for h in added.into_iter().rev() {
        let _ = ctl.stim(h, kind::HEAD, 0, 0, 0);
    }
    result
}

fn handles_viewer(c: &Ctx) -> R<()> {
    let mut r = raw_ready(c)?;
    let mut m = plain_request();
    m[2] |= wire::F_HANDLE as u8;
    let f = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
    r.send_with_fd(&m, &f)?;
    let e = r.error(WAIT)?;
    ensure!(
        e.code == wire::err::PROTOCOL,
        "expected PROTOCOL, got code {}",
        e.code
    );
    Ok(())
}

fn handles_bad_seen(c: &Ctx) -> R<()> {
    let mut r = raw_ready(c)?;
    r.send(&wire::handles_seen(1_000_000))?;
    let e = r.error(WAIT)?;
    ensure!(
        e.code == wire::err::PROTOCOL,
        "expected PROTOCOL for a count above the sent, got {}",
        e.code
    );
    let mut r = raw_ready(c)?;
    let n = r.reader.handles_parsed;
    r.send(&wire::handles_seen(n))?;
    r.send(&wire::handles_seen(n - 1))?;
    let e = r.error(WAIT)?;
    ensure!(
        e.code == wire::err::PROTOCOL,
        "expected PROTOCOL for a count going backwards, got {}",
        e.code
    );
    Ok(())
}

fn map_rw(fd: i32, size: u64) -> bool {
    // SAFETY: a probe mapping, unmapped at once.
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            size as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        );
        if p == libc::MAP_FAILED {
            false
        } else {
            libc::munmap(p, size as usize);
            true
        }
    }
}

fn regions_seals(c: &Ctx) -> R<()> {
    for rw in [false, true] {
        let mut r = Raw::connect(&c.t)?;
        let mut h = Raw::hello(1);
        if rw {
            h.features |= wire::feat::RW_PIXELS;
        }
        r.send(&h.encode())?;
        let end = Instant::now() + WAIT;
        while r.count(|m| matches!(m, ToViewer::Ready)) == 0 {
            ensure!(Instant::now() < end, "no READY");
            r.read(ms(50))?;
        }
        let mut slots = 0;
        for (m, _, _, fd) in &r.received {
            let ToViewer::Region(reg) = m else { continue };
            ensure!(
                reg.size % wire::REGION_ALIGN == 0,
                "region {} size {} is not a multiple of 64 KiB",
                reg.id,
                reg.size
            );
            let fd = fd.as_ref().unwrap().as_raw_fd();
            // SAFETY: fcntl and ftruncate on a descriptor we hold.
            let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
            ensure!(seals >= 0, "region {} is not a sealed memfd", reg.id);
            let need = 1 | 2 | 4;
            ensure!(
                seals & need == need,
                "region {} seals {seals:#x} lack SHRINK, GROW or SEAL",
                reg.id
            );
            ensure!(
                unsafe { libc::ftruncate(fd, (reg.size / 2) as libc::off_t) } != 0,
                "region {} can be shrunk",
                reg.id
            );
            ensure!(
                unsafe { libc::ftruncate(fd, (reg.size * 2) as libc::off_t) } != 0,
                "region {} can be grown",
                reg.id
            );
            let writable_expected =
                reg.kind == wire::KIND_FEEDBACK || (reg.kind == wire::KIND_SLOT && rw);
            if reg.kind == wire::KIND_SLOT {
                slots += 1;
            }
            let ro = seals & 16 != 0;
            ensure!(
                ro != writable_expected,
                "region {} kind {} FUTURE_WRITE seal is {ro}",
                reg.id,
                reg.kind
            );
            ensure!(
                map_rw(fd, reg.size) == writable_expected,
                "region {} kind {}: writable mapping {} (expected {writable_expected})",
                reg.id,
                reg.kind,
                !writable_expected
            );
        }
        ensure!(slots >= 2, "fewer than 2 slots");
    }
    Ok(())
}

// --- flow ---

/// Request until a frame shows (`pat`, `step`). A producer that composes
/// asynchronously may first serve the picture from before the stimulus.
fn settle(v: &mut V, pat: u32, step: u32) -> R<FrameLease> {
    let end = Instant::now() + WAIT;
    loop {
        let f = v.get(0, false)?;
        let pic = picture(&f);
        if step_of(&pic) == step
            && pic == testext::pattern_picture(pat, SEED, step, f.width, f.height)
        {
            return Ok(f);
        }
        ensure!(
            Instant::now() < end,
            "the picture never became step {step} of pattern {pat}"
        );
    }
}

/// Apply a stimulus, wait for the producer to settle and return the frame showing it.
fn start_pat(v: &mut V, pat: u32, step: u32) -> R<FrameLease> {
    v.pat(0, pat, SEED, step)?;
    let f = settle(v, pat, step)?;
    std::thread::sleep(ms(60));
    Ok(f)
}

fn lent_no_write(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    let mut b = V::connect(&c.t, &Opts::default().no_input())?;
    let fa = start_pat(&mut a, pattern::LINES, 0)?;
    let hash = |f: &FrameLease| {
        f.pixels()
            .iter()
            .fold(0u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
    };
    let (before, serial) = (hash(&fa), fa.frame.serial);
    for i in 1..=100 {
        b.pat(0, pattern::LINES, SEED, i)?;
        let f = b.get(0, false)?;
        verify(&f, pattern::LINES, SEED)?;
    }
    ensure!(
        hash(&fa) == before,
        "slot of lent frame {serial} changed while another viewer drove 100 steps"
    );
    Ok(())
}

fn credit_ceiling(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default().credit(2))?;
    let mut held = vec![];
    let mut step = 0;
    for _ in 0..12 {
        step += 1;
        v.pat(0, pattern::SOLID, SEED, step)?;
        v.s.request(0, false).map_err(|e| e.to_string())?;
        for f in v.collect(ms(120))? {
            held.push(f);
        }
        ensure!(held.len() <= 2, "{} leases held with credit 2", held.len());
    }
    ensure!(
        held.len() == 2,
        "expected 2 frames, received {}",
        held.len()
    );
    let s = v.stats()?;
    // Frames were queued, so a count of 0 means the counter was read from the wrong bytes.
    ensure!(
        s.max_queued >= 1,
        "STATS max_queued is 0 after {} frames: wrong STATS layout?",
        held.len()
    );
    ensure!(
        s.max_queued <= 1,
        "{} frames were wholly queued at once",
        s.max_queued
    );
    Ok(())
}

fn flow_last_frame(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    let f = start_pat(&mut v, pattern::SOLID, 1)?;
    let n = f.frame.serial;
    v.pat(0, pattern::SOLID, SEED, 2)?;
    v.s.request(0, false).map_err(|e| e.to_string())?;
    v.quiet(QUIET, "credit is used up")?;
    f.release();
    let g = v.frame_or_fail("after the release re-sent the request")?;
    ensure!(
        g.frame.serial > n,
        "serial {} is not newer than {n}",
        g.frame.serial
    );
    Ok(())
}

fn flow_pending(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut v, pattern::SOLID, 1)?.release_quiet();
    v.pat(0, pattern::SOLID, SEED, 2)?;
    v.sync()?;
    std::thread::sleep(ms(100));
    let f = v.get(0, false)?;
    // An asynchronous producer may serve the old picture first; it must still catch up.
    let f = if step_of(&picture(&f)) == 2 {
        f
    } else {
        settle(&mut v, pattern::SOLID, 2)?
    };
    verify(&f, pattern::SOLID, SEED)?;
    Ok(())
}

/// Add a test head and return its number, as the producer announces it.
fn add_head(ctl: &mut V) -> R<u32> {
    let n0 = ctl.s.head_count() as u32;
    ctl.stim(n0, kind::HEAD, 0, 0, 1)?;
    ctl.sync()?;
    let n1 = ctl.s.head_count() as u32;
    ensure!(n1 > n0, "no HEAD announced a new test head");
    Ok(n1 - 1)
}

fn flow_still_head(c: &Ctx) -> R<()> {
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    let h = add_head(&mut ctl)?;
    let r = (|| -> R<()> {
        let mut v = V::connect(&c.t, &Opts::default().no_input())?;
        let f = v.get(h, false)?;
        ensure!(f.frame.serial >= 1, "serial {}", f.frame.serial);
        ensure!(
            f.frame.damage == Damage::Whole,
            "first frame damage is {:?}",
            f.frame.damage
        );
        Ok(())
    })();
    let _ = ctl.stim(h, kind::HEAD, 0, 0, 0);
    r
}

fn flow_rehome_lease(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    let f = start_pat(&mut v, pattern::SOLID, 1)?;
    let (n, seq) = (f.frame.serial, f.frame.lease_seq);
    f.release_quiet();
    v.send(&wire::import_failed(rehome_region(&v)?, 3))?;
    v.item(WAIT, |i| matches!(i, Item::Head(_)).then_some(()))?
        .ok_or("no HEAD after IMPORT_FAILED")?;
    v.s.request(0, false).map_err(|e| e.to_string())?;
    let g = v.frame_or_fail("the re-homed viewer on a still screen")?;
    ensure!(
        g.frame.serial == n,
        "serial {} after re-home, expected {n}",
        g.frame.serial
    );
    ensure!(
        g.frame.lease_seq > seq,
        "lease_seq {} did not grow past {seq}",
        g.frame.lease_seq
    );
    ensure!(
        g.frame.damage == Damage::Whole,
        "damage {:?}",
        g.frame.damage
    );
    let hash = g.pixels().to_vec();
    v.pat(0, pattern::SOLID, SEED, 2)?;
    v.s.request(0, false).map_err(|e| e.to_string())?;
    v.quiet(QUIET, "the held slot has used the credit")?;
    ensure!(g.pixels() == hash, "held slot changed");
    Ok(())
}

fn rehome_region(v: &V) -> R<u32> {
    v.head(0)?
        .slot_region
        .first()
        .copied()
        .ok_or_else(|| "head 0 has no slots".to_string())
}

fn flow_race_mark(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut v, pattern::LINES, 0)?.release_quiet();
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    ctl.control(op::DELAYED_MARK, 0, 0, 40_000)?;
    ctl.pat(0, pattern::LINES, SEED, 3)?;
    ctl.sync()?;
    v.s.request(0, false).map_err(|e| e.to_string())?;
    let f = v.frame_or_fail("a request that landed between the mark's bit and flag")?;
    let f = if step_of(&picture(&f)) == 3 {
        f
    } else {
        settle(&mut v, pattern::LINES, 3)?
    };
    verify(&f, pattern::LINES, SEED)?;
    Ok(())
}

fn flow_unchanged_newer(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    let mut b = V::connect(&c.t, &Opts::default().no_input())?;
    let fa = start_pat(&mut a, pattern::SOLID, 1)?;
    let n = fa.frame.serial;
    fa.release_quiet();
    b.pat(0, pattern::SOLID, SEED, 2)?;
    let fb = settle(&mut b, pattern::SOLID, 2)?;
    ensure!(
        fb.frame.serial > n,
        "viewer B's frame {} is not newer than {n}",
        fb.frame.serial
    );
    std::thread::sleep(ms(30));
    let g = a.get(0, true)?;
    ensure!(
        g.frame.serial == fb.frame.serial,
        "A got serial {}, expected {}",
        g.frame.serial,
        fb.frame.serial
    );
    Ok(())
}

fn flow_no_unchanged(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut v, pattern::SOLID, 1)?.release_quiet();
    std::thread::sleep(ms(30));
    let s0 = v.stats()?;
    v.s.request(0, true).map_err(|e| e.to_string())?;
    v.quiet(ms(400), "SAMPLE_NOW with nothing changed")?;
    let s1 = v.stats()?;
    ensure!(
        s1.publishes == s0.publishes,
        "N rose from {} to {} with no change",
        s0.publishes,
        s1.publishes
    );
    Ok(())
}

fn flow_retries(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    let (mut last_serial, mut last_seq) = (0, 0);
    for i in 1..=45u32 {
        v.pat(0, pattern::LINES, SEED, i)?;
        let f = v.get(0, i % 2 == 0)?;
        ensure!(
            f.frame.serial > last_serial,
            "serial {} after {last_serial}",
            f.frame.serial
        );
        ensure!(
            f.frame.lease_seq > last_seq,
            "lease_seq {} after {last_seq}",
            f.frame.lease_seq
        );
        (last_serial, last_seq) = (f.frame.serial, f.frame.lease_seq);
        match i % 3 {
            0 => {
                f.release_quiet();
                v.s.request(0, false).map_err(|e| e.to_string())?;
            }
            1 => {
                v.s.request(0, false).map_err(|e| e.to_string())?;
                f.release_quiet();
            }
            _ => f.release(),
        }
        for g in v.collect(ms(20))? {
            ensure!(
                g.frame.serial > last_serial && g.frame.lease_seq > last_seq,
                "a retry re-delivered serial {}",
                g.frame.serial
            );
            (last_serial, last_seq) = (g.frame.serial, g.frame.lease_seq);
        }
    }
    Ok(())
}

// --- SAMPLE_NOW ---

fn sn_basic(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut v, pattern::LINES, 0)?.release_quiet();
    for i in 1..=25u32 {
        std::thread::sleep(ms(25));
        v.pat(0, pattern::LINES, SEED, i)?;
        let f = v.get(0, true)?;
        let (step, _) = verify(&f, pattern::LINES, SEED)?;
        ensure!(step >= i, "SAMPLE_NOW after step {i} returned step {step}");
    }
    Ok(())
}

fn sn_bound(c: &Ctx) -> R<()> {
    for credit in [1, 2] {
        let mut a = V::connect(&c.t, &Opts::default().credit(credit))?;
        let mut b = V::connect(&c.t, &Opts::default().no_input())?;
        let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
        start_pat(&mut a, pattern::SOLID, 1)?.release_quiet();
        b.pat(0, pattern::SOLID, SEED, 2)?;
        b.get(0, false)?.release_quiet();
        std::thread::sleep(ms(40));
        ensure!(ctl.control(op::HOLD_SAMPLE, 0, 0, 0)? == 0, "hold refused");
        a.pat(0, pattern::SOLID, SEED, 3)?;
        a.s.request(0, true).map_err(|e| e.to_string())?;
        if credit == 2 {
            a.s.request(0, false).map_err(|e| e.to_string())?;
        }
        a.quiet(ms(300), "SAMPLE_NOW while its sample is held")?;
        ensure!(
            ctl.control(op::RELEASE_SAMPLE, 0, 0, 0)? == 0,
            "no sample was held"
        );
        let f = a.frame_or_fail("after the held sample completed")?;
        let (step, _) = verify(&f, pattern::SOLID, SEED)?;
        ensure!(
            step == 3,
            "frame shows step {step}, expected A's step 3 (credit {credit})"
        );
    }
    // No free credit: nothing is sampled for the request, and it is bound only when credit frees.
    let mut a = V::connect(&c.t, &Opts::default())?;
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    let held = start_pat(&mut a, pattern::SOLID, 4)?;
    let s0 = ctl.stats()?;
    a.pat(0, pattern::SOLID, SEED, 5)?;
    a.s.request(0, true).map_err(|e| e.to_string())?;
    std::thread::sleep(ms(250));
    let s1 = ctl.stats()?;
    ensure!(
        s1.samples == s0.samples,
        "{} samples ran for a viewer with no free credit",
        s1.samples - s0.samples
    );
    ensure!(ctl.control(op::HOLD_SAMPLE, 0, 0, 0)? == 0, "hold refused");
    held.release();
    a.quiet(ms(300), "the bound sample is held")?;
    ctl.control(op::RELEASE_SAMPLE, 0, 0, 0)?;
    let f = a.frame_or_fail("after credit freed and the sample completed")?;
    let (step, _) = verify(&f, pattern::SOLID, SEED)?;
    ensure!(step == 5, "frame shows step {step}, expected 5");
    Ok(())
}

// --- pixels ---

fn pix_damage(c: &Ctx) -> R<()> {
    for pat in [pattern::SOLID, pattern::BAR, pattern::LINES] {
        let mut v = V::connect(&c.t, &Opts::default())?;
        let first = v.get(0, false)?;
        ensure!(
            first.frame.damage == Damage::Whole,
            "a new viewer's first frame has damage {:?}",
            first.frame.damage
        );
        first.release_quiet();
        let f = start_pat(&mut v, pat, 0)?;
        let (_, mut prev) = verify(&f, pat, SEED)?;
        f.release();
        for i in 1..=30u32 {
            v.pat(0, pat, SEED, i * if pat == pattern::LINES { 1 } else { 3 })?;
            let f = v.get(0, false)?;
            let (_, cur) = verify(&f, pat, SEED)?;
            check_damage(&prev, &cur, &f)?;
            prev = cur;
        }
    }
    Ok(())
}

fn pix_complete(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut a, pattern::LINES, 99)?.release_quiet();
    let mut b = V::connect(&c.t, &Opts::default().credit(2).no_input())?;
    let (mut pa, mut pb): (Option<Vec<u32>>, Option<Vec<u32>>) = (None, None);
    let mut held = std::collections::VecDeque::new();
    for i in 0..60u32 {
        a.pat(0, pattern::LINES, SEED, i)?;
        let f = a.get(0, false)?;
        let (_, cur) = verify(&f, pattern::LINES, SEED)?;
        if let Some(p) = &pa {
            check_damage(p, &cur, &f)?;
        }
        pa = Some(cur);
        f.release();
        if i % 2 == 0 {
            let g = b.get(0, false)?;
            let (_, cur) = verify(&g, pattern::LINES, SEED)?;
            if let Some(p) = &pb {
                check_damage(p, &cur, &g)?;
            }
            pb = Some(cur);
            held.push_back(g);
            if held.len() > 1 {
                held.pop_front();
            }
        }
    }
    Ok(())
}

fn pix_two_sets(c: &Ctx) -> R<()> {
    let mut ro = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut ro, pattern::LINES, 99)?.release_quiet();
    let mut rw = V::connect(&c.t, &Opts::default().rw().no_input())?;
    let steps = [
        0u32, 1, 2, 3, 9, 10, 4, 5, 3, 11, 12, 2, 13, 14, 6, 7, 20, 21, 8, 22,
    ];
    let (mut p1, mut p2): (Option<Vec<u32>>, Option<Vec<u32>>) = (None, None);
    for (k, &s) in steps.iter().enumerate() {
        ro.pat(0, pattern::LINES, SEED, s)?;
        let (v, prev) = if k % 2 == 0 {
            (&mut ro, &mut p1)
        } else {
            (&mut rw, &mut p2)
        };
        let f = v.get(0, false)?;
        let (_, cur) = verify(&f, pattern::LINES, SEED)?;
        if let Some(p) = prev {
            check_damage(p, &cur, &f)?;
        }
        *prev = Some(cur);
    }
    Ok(())
}

fn pix_concurrent(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    let mut b = V::connect(&c.t, &Opts::default().rw().no_input())?;
    let mut d = V::connect(&c.t, &Opts::default().credit(2).no_input())?;
    start_pat(&mut a, pattern::LINES, 99)?.release_quiet();
    let t = c.t.clone();
    let stepper = std::thread::spawn(move || -> R<()> {
        let s = V::connect(&t, &Opts::default().no_input())?;
        let end = Instant::now() + ms(1000);
        let mut i = 0u32;
        while Instant::now() < end {
            s.pat(0, pattern::LINES, SEED, i)?;
            i += 1;
            if i.is_multiple_of(8) {
                std::thread::sleep(ms(1));
            }
        }
        s.s.socket().shutdown(std::net::Shutdown::Both).ok();
        Ok(())
    });
    let end = Instant::now() + ms(1200);
    let mut seen = 0;
    while Instant::now() < end {
        for v in [&mut a, &mut b, &mut d] {
            v.s.request(0, false).map_err(|e| e.to_string())?;
            if let Some(f) = v.frame(ms(40))? {
                verify(&f, pattern::LINES, SEED)?;
                seen += 1;
            }
        }
    }
    ensure!(
        seen >= 3,
        "only {seen} frames reached three viewers in 1.2 s"
    );
    stepper
        .join()
        .map_err(|_| "stepper panicked".to_string())??;
    Ok(())
}

// --- sets ---

fn sets_slot_list(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default().credit(2))?;
    let f1 = start_pat(&mut a, pattern::SOLID, 1)?;
    a.pat(0, pattern::SOLID, SEED, 2)?;
    let f2 = a.get(0, false)?;
    let (h1, h2) = (f1.pixels().to_vec(), f2.pixels().to_vec());
    let before = a.head(0)?.mode_serial;
    {
        let b = V::connect(&c.t, &Opts::default().no_input())?;
        b.s.socket().shutdown(std::net::Shutdown::Both).ok();
    }
    std::thread::sleep(ms(200));
    a.collect(ms(100))?;
    ensure!(
        f1.pixels() == h1 && f2.pixels() == h2,
        "a held slot changed when another viewer joined and left"
    );
    ensure!(
        a.head(0)?.mode_serial == before,
        "mode_serial changed with the slot list"
    );
    f2.release();
    f1.release();
    a.pat(0, pattern::SOLID, SEED, 3)?;
    verify(&a.get(0, false)?, pattern::SOLID, SEED)?;
    Ok(())
}

fn sets_head_resets_nothing(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut a, pattern::SOLID, 1)?.release_quiet();
    a.s.request(0, false).map_err(|e| e.to_string())?;
    a.sync()?;
    a.quiet(QUIET, "nothing changed")?;
    let _b = V::connect(&c.t, &Opts::default().credit(2).no_input())?;
    a.quiet(ms(500), "a second viewer joined and resized the set")?;
    Ok(())
}

fn sets_rehoming(c: &Ctx) -> R<()> {
    for reason in [3u32, 7] {
        let mut v = V::connect(&c.t, &Opts::default())?;
        let f = start_pat(&mut v, pattern::SOLID, 1)?;
        let old = v.head(0)?.slot_region.clone();
        let n = f.frame.serial;
        f.release_quiet();
        v.send(&wire::import_failed(old[0], reason))?;
        let h = v
            .item(WAIT, |i| {
                if let Item::Head(h) = i {
                    Some(h.clone())
                } else {
                    None
                }
            })?
            .ok_or("no HEAD")?;
        if reason == 3 {
            ensure!(
                h.slot_region.iter().all(|r| !old.contains(r)),
                "reason 3 kept the old regions"
            );
        }
        v.s.request(0, false).map_err(|e| e.to_string())?;
        let g = v.frame_or_fail("after re-homing on a still screen")?;
        ensure!(
            g.frame.serial == n && g.frame.damage == Damage::Whole,
            "reason {reason}: serial {} damage {:?}",
            g.frame.serial,
            g.frame.damage
        );
        verify(&g, pattern::SOLID, SEED)?;
        g.release_quiet();
        v.send(&wire::set_storage(0, [1, 0, 0, 0, 0, 0, 0, 0]))?;
        v.item(WAIT, |i| matches!(i, Item::Head(_)).then_some(()))?
            .ok_or("no HEAD after SET_STORAGE")?;
        v.s.request(0, false).map_err(|e| e.to_string())?;
        v.frame_or_fail("after SET_STORAGE")?;
    }
    let mut v = V::connect(&c.t, &Opts::default())?;
    let f = start_pat(&mut v, pattern::SOLID, 1)?;
    let r = f.frame.region;
    f.release_quiet();
    v.send(&wire::import_failed(r, 5))?;
    let h = v
        .item(WAIT, |i| {
            if let Item::Head(h) = i {
                Some(h.clone())
            } else {
                None
            }
        })?
        .ok_or("no HEAD after reason 5")?;
    ensure!(
        h.flags & wire::HEAD_TOO_LARGE != 0 && h.slot_region.is_empty(),
        "reason 5 did not mark the head too large"
    );
    Ok(())
}

// --- heads ---

fn heads_mode(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    start_pat(&mut v, pattern::SOLID, 1)?.release_quiet();
    let ms0 = v.head(0)?.mode_serial;
    let (nw, nh) = (c.w + 8, c.h + 8);
    let res = (|| -> R<()> {
        ensure!(ctl.control(op::HOLD_SAMPLE, 0, 0, 0)? == 0, "hold refused");
        ctl.stim(0, kind::MODE, nw as u64 | (nh as u64) << 16, 0, 0)?;
        ctl.sync()?;
        v.s.request(0, false).map_err(|e| e.to_string())?;
        v.quiet(ms(300), "the new mode's first sample is held")?;
        ensure!(
            ctl.control(op::RELEASE_SAMPLE, 0, 0, 0)? == 0,
            "no sample held"
        );
        let h = v.head(0)?;
        ensure!(
            h.mode_serial != ms0 && h.width == nw && h.height == nh,
            "HEAD did not change: {h:?}"
        );
        let f = v.frame_or_fail("the new mode's first frame")?;
        ensure!(
            f.frame.mode_serial == h.mode_serial,
            "FRAME carries the old mode"
        );
        ensure!(
            f.frame.damage == Damage::Whole,
            "damage {:?}",
            f.frame.damage
        );
        ensure!(
            f.width == nw && f.pixels().chunks(4).take(1).all(|p| p[..3] == [0, 0, 0]),
            "new mode is not black"
        );
        Ok(())
    })();
    let _ = ctl.stim(0, kind::MODE, c.w as u64 | (c.h as u64) << 16, 0, 0);
    res?;
    // A removed head's requests are dropped.
    let mut a = V::connect(&c.t, &Opts::default().no_input())?;
    let hn = add_head(&mut a)?;
    let mut b = V::connect(&c.t, &Opts::default().no_input())?;
    b.get(hn, false)?.release_quiet();
    b.s.request(hn, false).map_err(|e| e.to_string())?;
    a.stim(hn, kind::HEAD, 0, 0, 0)?;
    a.sync()?;
    b.quiet(ms(300), "the head was removed")?;
    Ok(())
}

fn heads_disconnect(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default().credit(2))?;
    let mut b = V::connect(&c.t, &Opts::default().no_input())?;
    let f = start_pat(&mut a, pattern::LINES, 0)?;
    let kept = f.slot.clone();
    let len = f.stride as usize * f.height as usize;
    // SAFETY: the lease is still alive here.
    let snap: Vec<u8> = unsafe { kept.bytes(0, len) }.to_vec();
    std::mem::forget(f);
    b.pat(0, pattern::LINES, SEED, 1)?;
    b.get(0, false)?.release_quiet();
    let s0 = b.stats()?;
    a.s.socket().shutdown(std::net::Shutdown::Both).ok();
    drop(a);
    std::thread::sleep(ms(100));
    for i in 2..40u32 {
        b.pat(0, pattern::LINES, SEED, i)?;
        verify(&b.get(0, false)?, pattern::LINES, SEED)?;
    }
    // SAFETY: only our own read of a retired mapping.
    ensure!(
        unsafe { kept.bytes(0, len) } == &snap[..],
        "a retired slot changed after its viewer left"
    );
    let s1 = b.stats()?;
    let per = (s1.lines_stored - s0.lines_stored) / (s1.publishes - s0.publishes).max(1);
    ensure!(
        per < c.h as u64,
        "{per} lines stored per publish: every publish rewrote the whole head"
    );
    Ok(())
}

fn heads_cancel(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    let f = start_pat(&mut a, pattern::LINES, 0)?;
    let (_, prev) = verify(&f, pattern::LINES, SEED)?;
    f.release_quiet();
    ensure!(
        ctl.control(op::HOLD_WRITES, 0, a.id, 0)? == 0,
        "hold refused"
    );
    ctl.pat(0, pattern::LINES, SEED, 5)?;
    ctl.sync()?;
    a.s.request(0, false).map_err(|e| e.to_string())?;
    std::thread::sleep(ms(150));
    ensure!(
        ctl.control(op::RETIRE_QUEUED, 0, a.id, 0)? == 0,
        "no wholly queued FRAME to retire"
    );
    a.s.request(0, false).map_err(|e| e.to_string())?;
    a.sync_blind();
    ensure!(
        ctl.control(op::RESUME_WRITES, 0, a.id, 0)? == 0,
        "resume refused"
    );
    let g = a.frame_or_fail("a fresh frame after the cancelled one")?;
    let (step, cur) = verify(&g, pattern::LINES, SEED)?;
    ensure!(step == 5, "step {step}");
    check_damage(&prev, &cur, &g)?;
    a.quiet(QUIET, "one frame answers both requests")?;
    Ok(())
}

fn heads_cancel_rehome(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    let f = start_pat(&mut a, pattern::SOLID, 1)?;
    let (n, seq) = (f.frame.serial, f.frame.lease_seq);
    let r = f.frame.region;
    f.release_quiet();
    a.send(&wire::import_failed(r, 3))?;
    a.item(WAIT, |i| matches!(i, Item::Head(_)).then_some(()))?
        .ok_or("no HEAD after IMPORT_FAILED")?;
    ensure!(
        ctl.control(op::HOLD_WRITES, 0, a.id, 0)? == 0,
        "hold refused"
    );
    a.s.request(0, false).map_err(|e| e.to_string())?;
    a.sync_blind();
    ensure!(
        ctl.control(op::RETIRE_QUEUED, 0, a.id, 0)? == 0,
        "no wholly queued FRAME to retire"
    );
    ensure!(
        ctl.control(op::RESUME_WRITES, 0, a.id, 0)? == 0,
        "resume refused"
    );
    let g = a.frame_or_fail("the re-homed viewer after a cancelled first frame")?;
    ensure!(
        g.frame.serial == n && g.frame.lease_seq > seq,
        "serial {} lease_seq {}",
        g.frame.serial,
        g.frame.lease_seq
    );
    ensure!(
        g.frame.damage == Damage::Whole,
        "damage {:?}",
        g.frame.damage
    );
    Ok(())
}

// --- cursor, state, time ---

fn cursor_all(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default().no_input())?;
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    start_pat(&mut v, pattern::SOLID, 1)?.release_quiet();
    let cur = v.s.state.cursor(0).ok_or("no cursor record")?;
    v.s.request_cursor(0, cur.seq).map_err(|e| e.to_string())?;
    v.sync()?;
    ctl.stim(0, kind::CURSOR_MOVE, 0x0010_0010, 0, 1)?;
    let seq = v
        .item(WAIT, |i| {
            if let Item::CursorNotify { seq, .. } = i {
                Some(*seq)
            } else {
                None
            }
        })?
        .ok_or("no CURSOR_NOTIFY after a move")?;
    let rec = v.s.state.cursor(0).unwrap();
    ensure!(
        rec.seq == seq && rec.x == 16 && rec.y == 16 && rec.flags & 1 != 0,
        "cursor record {rec:?}"
    );
    ctl.stim(0, kind::CURSOR_MOVE, 0x0020_0020, 0, 1)?;
    ctl.sync()?;
    ensure!(
        v.item(ms(300), |i| matches!(i, Item::CursorNotify { .. })
            .then_some(()))?
            .is_none(),
        "a second notify without a new request"
    );
    ctl.stim(0, kind::CURSOR_IMAGE, 0xAABB_CCDD, 0, 4)?;
    let img = v
        .item(WAIT, |i| {
            if let Item::CursorImage(c) = i {
                Some(c.clone())
            } else {
                None
            }
        })?
        .ok_or("no CURSOR_IMAGE")?;
    ensure!(
        img.width == 4
            && img.height == 4
            && img.pixels.chunks(4).all(|p| p == [0xDD, 0xCC, 0xBB, 0xAA]),
        "image {img:?}"
    );
    // A seqlock reader never sees x != y while the source writes x == y.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let t = c.t.clone();
    let s2 = stop.clone();
    let writer = std::thread::spawn(move || -> R<()> {
        let w = V::connect(&t, &Opts::default().no_input())?;
        let mut i = 0u32;
        while !s2.load(std::sync::atomic::Ordering::Relaxed) {
            let p = (i & 0x3FF) as u64;
            w.stim(0, kind::CURSOR_MOVE, p | p << 16, 0, 1)?;
            i += 1;
        }
        Ok(())
    });
    let end = Instant::now() + ms(500);
    let mut bad = None;
    while Instant::now() < end {
        let r = v.s.state.cursor(0).unwrap();
        if r.x != r.y {
            bad = Some(r);
            break;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().map_err(|_| "writer panicked")??;
    ensure!(bad.is_none(), "torn cursor record {bad:?}");
    Ok(())
}

fn state_record(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    let t = c.t.clone();
    let writer = std::thread::spawn(move || -> R<()> {
        let w = V::connect(&t, &Opts::default().no_input())?;
        for i in 0..300u32 {
            w.pat(0, pattern::SOLID, SEED, i)?;
            std::thread::sleep(ms(1));
        }
        Ok(())
    });
    let mut last = 0;
    let end = Instant::now() + ms(600);
    while Instant::now() < end {
        v.s.request(0, false).map_err(|e| e.to_string())?;
        if let Some(f) = v.frame(ms(50))? {
            let r = v.s.state.frame(0).ok_or("no frame record")?;
            ensure!(
                r.serial >= f.frame.serial,
                "state serial {} behind FRAME {}",
                r.serial,
                f.frame.serial
            );
            ensure!(
                r.sample_ns <= r.publish_ns,
                "torn record: sample {} after publish {}",
                r.sample_ns,
                r.publish_ns
            );
            ensure!(
                r.serial >= last,
                "state serial went back from {last} to {}",
                r.serial
            );
            last = r.serial;
        }
    }
    writer.join().map_err(|_| "writer panicked")??;
    Ok(())
}

fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn time_stamps(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    let (mut serial, mut seq) = (0, 0);
    for i in 0..20u32 {
        v.pat(0, pattern::SOLID, SEED, i)?;
        let f = v.get(0, false)?;
        let rx = now_ns();
        let fr = &f.frame;
        ensure!(
            fr.sample_ns <= fr.publish_ns && fr.publish_ns <= rx,
            "sample {} publish {} receipt {rx}",
            fr.sample_ns,
            fr.publish_ns
        );
        ensure!(
            fr.serial >= serial && fr.lease_seq > seq,
            "serial {} (was {serial}), lease_seq {} (was {seq})",
            fr.serial,
            fr.lease_seq
        );
        (serial, seq) = (fr.serial, fr.lease_seq);
    }
    Ok(())
}

// --- input ---

impl V {
    /// Send a `SYNC` asking for an acknowledgement and wait for it.
    fn ack(&mut self, serial: u32) -> R<()> {
        self.send(&wire::sync(1, serial))?;
        self.item(WAIT, |i| {
            matches!(i, Item::InputAck(s) if *s == serial).then_some(())
        })?
        .ok_or_else(|| "no INPUT_ACK".to_string())
    }

    pub fn sync_blind(&mut self) {
        std::thread::sleep(ms(120));
    }

    fn echoes(&mut self) -> Vec<testext::InputEcho> {
        let v: Vec<_> = self
            .log
            .iter()
            .filter_map(|i| {
                if let Item::Echo(e) = i {
                    Some(*e)
                } else {
                    None
                }
            })
            .collect();
        self.log.clear();
        v
    }
}

fn kinds(e: &[testext::InputEcho]) -> Vec<u32> {
    e.iter().map(|x| x.kind).collect()
}

fn input_reports(c: &Ctx) -> R<()> {
    let mut v = V::connect(&c.t, &Opts::default())?;
    v.send(&wire::input_claim(true))?;
    v.ack(1)?;
    v.echoes();
    // A press and a release are two reports.
    v.send(&wire::button(0, 0, 1))?;
    v.send(&wire::button(0, 0, 0))?;
    v.ack(2)?;
    let e = v.echoes();
    ensure!(
        kinds(&e)
            == [
                echo::BUTTON,
                echo::REPORT_END,
                echo::BUTTON,
                echo::REPORT_END
            ],
        "button echoes {:?}",
        kinds(&e)
    );
    // 300 events split at 256.
    for i in 0..300u32 {
        v.send(&wire::key(0, 30, (i % 2 == 0) as u32))?;
    }
    v.ack(3)?;
    let e = v.echoes();
    let ends: Vec<usize> = e
        .iter()
        .enumerate()
        .filter(|(_, x)| x.kind == echo::REPORT_END)
        .map(|(i, _)| i)
        .collect();
    ensure!(
        ends == [256, 301],
        "report ends at {ends:?} in {} echoes",
        e.len()
    );
    // Relative motion sums, clamps and keeps order around a key.
    v.send(&wire::rel(0, 10, 0))?;
    v.send(&wire::rel(0, 20, 5))?;
    v.send(&wire::key(0, 31, 1))?;
    v.send(&wire::rel(0, 40_000, -40_000))?;
    v.send(&wire::rel(0, 40_000, -40_000))?;
    v.send(&wire::key(0, 31, 0))?;
    v.ack(4)?;
    let e = v.echoes();
    let s: Vec<(u32, i32, i32)> = e
        .iter()
        .filter(|x| x.kind != echo::REPORT_END)
        .map(|x| (x.kind, x.a, x.b))
        .collect();
    ensure!(
        s.len() == 4
            && s[..2] == [(echo::REL, 30, 5), (echo::KEY, 1, 0)]
            && s[3] == (echo::KEY, 0, 0),
        "relative motion echoes {s:?}"
    );
    // The sum clamps at 32767; a producer that rate-limits relative motion
    // (QEMU forwards at most 4096 counts per 10 ms, section 13.2) may give less.
    let (k, a, b) = s[2];
    ensure!(
        k == echo::REL
            && (a == 32767 || (1..=4096).contains(&a))
            && (b == -32767 || (-4096..=-1).contains(&b)),
        "clamped relative motion echoed {s:?}"
    );
    // Extremes and invalid values are dropped, not fatal.
    for m in [
        wire::key(0, 0x300, 1),
        wire::key(0, 0, 1),
        wire::key(0, 30, 2),
        wire::key(999, 30, 1),
        wire::button(0, 9, 1),
        wire::rel(0, i32::MIN, i32::MAX),
        wire::abs(0, i32::MIN, i32::MAX),
        wire::abs(0, i32::MAX, i32::MIN),
    ] {
        v.send(&m)?;
    }
    v.ack(5)?;
    let e = v.echoes();
    ensure!(
        !e.iter()
            .any(|x| x.kind == echo::KEY || x.kind == echo::BUTTON),
        "invalid key or button events were injected: {e:?}"
    );
    for x in e.iter().filter(|x| x.kind == echo::ABS) {
        ensure!(
            x.a >= 0 && x.a < c.w as i32 && x.b >= 0 && x.b < c.h as i32,
            "ABS {},{} outside the head",
            x.a,
            x.b
        );
    }
    Ok(())
}

fn input_owner(c: &Ctx) -> R<()> {
    let mut a = V::connect(&c.t, &Opts::default())?;
    a.send(&wire::input_claim(true))?;
    a.ack(1)?;
    let mut b = V::connect(&c.t, &Opts::default())?;
    b.send(&wire::key(0, 40, 1))?;
    b.send(&wire::sync(0, 0))?;
    b.sync()?;
    a.collect(ms(100))?;
    a.echoes();
    a.send(&wire::key(0, 41, 1))?;
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    let hn = add_head(&mut ctl)?;
    a.collect(ms(100))?;
    a.send(&wire::key(hn, 42, 1))?;
    a.ack(2)?;
    let e = a.echoes();
    ensure!(
        e.iter().all(|x| x.code != 40),
        "the non-owner's key reached the guest"
    );
    ensure!(
        e.iter().any(|x| x.kind == echo::KEY && x.code == 41),
        "the owner's key did not"
    );
    // Owner change releases what the old owner held.
    b.send(&wire::input_claim(true))?;
    b.ack(3)?;
    a.collect(ms(300))?;
    let e = a.echoes();
    ensure!(
        e.iter()
            .any(|x| x.kind == echo::KEY && x.code == 41 && x.a == 0),
        "owner change did not release the held key: {e:?}"
    );
    ensure!(
        e.iter()
            .any(|x| x.kind == echo::KEY && x.code == 42 && x.a == 0 && x.head == hn),
        "a key held on head {hn} was not released on that head: {e:?}"
    );
    let _ = ctl.stim(hn, kind::HEAD, 0, 0, 0);
    Ok(())
}

// --- limits and security ---

fn limits_overflow(c: &Ctx) -> R<()> {
    let mut ctl = V::connect(&c.t, &Opts::default().no_input())?;
    let mut r = Raw::connect(&c.t)?;
    r.handshake(1)?;
    let id = r
        .received
        .iter()
        .find_map(|(m, ..)| {
            if let ToViewer::Welcome(w) = m {
                Some(w.viewer_id)
            } else {
                None
            }
        })
        .unwrap();
    ensure!(ctl.control(op::HOLD_WRITES, 0, id, 0)? == 0, "hold refused");
    r.send(&wire::input_claim(true))?;
    let mut burst = Vec::new();
    for i in 0..4096u32 {
        burst.extend_from_slice(&wire::sync(1, i));
    }
    // 4 MiB of 16-byte acknowledgements is 262,144 reports.
    let budget: u64 = std::env::var("CONF_OVERFLOW_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45);
    let end = Instant::now() + Duration::from_secs(budget);
    let mut sent = 0u32;
    while Instant::now() < end && sent < 200 {
        if r.send(&burst).is_err() {
            break;
        }
        sent += 1;
        r.read(Duration::ZERO)?;
        if r.count(|m| matches!(m, ToViewer::Error(_))) > 0 {
            break;
        }
    }
    let timed_out = Instant::now() >= end;
    let _ = ctl.control(op::RESUME_WRITES, 0, id, 0);
    let err = match r.error(WAIT) {
        Ok(e) => e,
        Err(_) if timed_out => {
            return Err(format!(
                "skip: {} reports in {budget} s did not reach the 4 MiB queue limit (set CONF_OVERFLOW_SECS)",
                sent * 4096
            ))
        }
        Err(e) => return Err(format!("{e} (after {sent} bursts of 4096 reports)")),
    };
    ensure!(
        err.code == wire::err::LIMIT,
        "expected LIMIT, got code {}",
        err.code
    );
    Ok(())
}

fn limits_too_large(c: &Ctx) -> R<()> {
    let mut v = V::connect(
        &c.t,
        &Opts {
            max_w: 4,
            ..Opts::default()
        },
    )?;
    let h = v.head(0)?;
    ensure!(
        h.flags & wire::HEAD_TOO_LARGE != 0 && h.slot_region.is_empty(),
        "HEAD {h:?} is not TOO_LARGE"
    );
    v.s.request(0, false).map_err(|e| e.to_string())?;
    v.quiet(QUIET, "head beyond max_width")?;
    Ok(())
}

fn security_silent(c: &Ctx) -> R<()> {
    let idle = Raw::connect(&c.t)?;
    let mut v = V::connect(&c.t, &Opts::default())?;
    start_pat(&mut v, pattern::SOLID, 1)?;
    v.pat(0, pattern::SOLID, SEED, 2)?;
    std::thread::sleep(ms(100));
    drop(idle);
    Ok(())
}
