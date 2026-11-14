//! Evidence for a guest that stalls or a test that fails.
//!
//! A stalled IRIX install leaves little behind: the console is quiet and the
//! logs stop. This records, in one small directory, what tells the layers
//! apart: the sockets between QEMU and its network peer (queue sizes show
//! which side stopped reading), QEMU's own view of the guest, what every
//! thread of the guest's processes is waiting on, and, when an Instigator was
//! started for the run, a goroutine dump from it.
//!
//! Only this run's processes are looked at or signalled: the machine's runner
//! (its pid is in `state/run.json`), everything below it, and the Instigator
//! pid the caller passes. Nothing is chosen by program name, so another
//! session's QEMU on a shared host is never touched. Every step is bounded
//! and a failure is written down, never raised, because this runs while the
//! test is already failing.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::product::run_limited;
use super::Result;

/// The whole capture has to stay inside the CI step's grace period after a
/// timeout signal: `ss` gets 5 seconds, the QEMU monitor 15 in total and the
/// Instigator dump 2 more.
const SS_TIMEOUT: Duration = Duration::from_secs(5);
const HMP_BUDGET: Duration = Duration::from_secs(15);

fn write(path: &Path, text: &str) {
    let _ = fs::write(path, text);
}

fn step(dir: &Path, name: &str, f: impl FnOnce() -> Result<String>) {
    let text = match f() {
        Ok(t) => t,
        Err(e) => format!("capture failed: {e}\n"),
    };
    write(&dir.join(format!("{name}.txt")), &text);
}

fn run_record(machine: &Path) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(&fs::read_to_string(
        machine.join("state/run.json"),
    )?)?)
}

/// Save the evidence for the machine at `machine` into
/// `evidence/<name>-failure/`. `instigator` is the pid of an Instigator the
/// test started for this run, if any.
pub fn save(evidence: &Path, name: &str, machine: &Path, instigator: Option<u32>) {
    let out = evidence.join(format!("{name}-failure"));
    if fs::create_dir_all(&out).is_err() {
        return;
    }
    write(
        &out.join("when.txt"),
        &format!("{:?}\n", std::time::SystemTime::now()),
    );

    let runner = run_record(machine)
        .ok()
        .and_then(|r| r["pid"].as_u64())
        .map(|p| p as u32);
    let mut own: BTreeSet<u32> = BTreeSet::new();
    if let Some(r) = runner {
        own.extend(descendants(r));
    }
    own.extend(instigator);

    step(&out, "sockets", || sockets(&own));
    step(&out, "qemu-monitor", || {
        hmp(machine, &["info status", "info network", "info registers"])
    });
    step(&out, "threads", || threads(&own, runner, instigator));
    if let Some(pid) = instigator {
        step(&out, "instigator-sigquit", || quit_instigator(&out, pid));
    }
}

#[cfg(target_os = "linux")]
fn parents() -> Vec<(u32, u32)> {
    let mut all = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return all;
    };
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // The command name is parenthesised and may hold spaces.
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        if let Some(ppid) = rest.split_whitespace().nth(1).and_then(|s| s.parse().ok()) {
            all.push((pid, ppid));
        }
    }
    all
}

#[cfg(not(target_os = "linux"))]
fn parents() -> Vec<(u32, u32)> {
    // `ps -A -o pid=,ppid=` is the portable way to the same table.
    let mut cmd = Command::new("ps");
    cmd.args(["-A", "-o", "pid=,ppid="]);
    let Ok(out) = run_limited(cmd, SS_TIMEOUT) else {
        return Vec::new();
    };
    out.stdout
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace().map(|s| s.parse::<u32>());
            Some((f.next()?.ok()?, f.next()?.ok()?))
        })
        .collect()
}

/// `root` and every process below it, found by parent pid.
fn descendants(root: u32) -> BTreeSet<u32> {
    let table = parents();
    let mut found = BTreeSet::from([root]);
    loop {
        let before = found.len();
        for &(pid, ppid) in &table {
            if found.contains(&ppid) {
                found.insert(pid);
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

fn sockets(own: &BTreeSet<u32>) -> Result<String> {
    if cfg!(windows) {
        let mut cmd = Command::new("netstat");
        cmd.args(["-ano"]);
        return Ok(run_limited(cmd, SS_TIMEOUT)?.stdout);
    }
    if cfg!(target_os = "macos") {
        let mut cmd = Command::new("netstat");
        cmd.args(["-an", "-p", "tcp"]);
        return Ok(run_limited(cmd, SS_TIMEOUT)?.stdout);
    }
    let mut cmd = Command::new("ss");
    cmd.arg("-tanpi");
    let out = run_limited(cmd, SS_TIMEOUT)?.stdout;
    // A socket is its header line plus the indented detail lines under it.
    // Keep the sockets one of our processes holds, which includes the
    // other end of a loopback pair when that end is ours too.
    let mut keep = String::new();
    let mut entry = String::new();
    let flush = |entry: &mut String, keep: &mut String| {
        if own.iter().any(|p| entry.contains(&format!("pid={p},"))) {
            keep.push_str(entry);
        }
        entry.clear();
    };
    for line in out.lines() {
        if !line.starts_with(char::is_whitespace) {
            flush(&mut entry, &mut keep);
        }
        entry.push_str(line);
        entry.push('\n');
    }
    flush(&mut entry, &mut keep);
    let keep: String = keep
        .lines()
        .map(|l| format!("{}\n", l.trim_end()))
        .collect();
    Ok(format!("sockets held by pids {own:?}\n{keep}"))
}

#[cfg(target_os = "linux")]
fn threads(own: &BTreeSet<u32>, _runner: Option<u32>, _inst: Option<u32>) -> Result<String> {
    let mut text = String::new();
    for pid in own {
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        text.push_str(&format!("== pid {pid}: {}\n", super::head(&cmdline, 200)));
        let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        let mut tids: Vec<u32> = tasks
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .collect();
        tids.sort_unstable();
        for tid in tids {
            let base = format!("/proc/{pid}/task/{tid}");
            let comm = fs::read_to_string(format!("{base}/comm")).unwrap_or_default();
            let stat = fs::read_to_string(format!("{base}/stat")).unwrap_or_default();
            // The state is the field after the parenthesised command name.
            let state = stat
                .rsplit_once(')')
                .and_then(|(_, r)| r.split_whitespace().next())
                .unwrap_or("?");
            let wchan = fs::read_to_string(format!("{base}/wchan")).unwrap_or_default();
            text.push_str(&format!(
                "{tid} {} state={state} wchan={}\n",
                comm.trim(),
                if wchan.trim().is_empty() {
                    "-"
                } else {
                    wchan.trim()
                }
            ));
        }
    }
    Ok(text)
}

#[cfg(not(target_os = "linux"))]
fn threads(own: &BTreeSet<u32>, _runner: Option<u32>, _inst: Option<u32>) -> Result<String> {
    Ok(format!(
        "per-thread states are read from /proc, which this OS lacks; pids {own:?}\n"
    ))
}

/// Run HMP commands through QMP's `human-monitor-command`. The CLI has no
/// monitor command, but QMP's loopback port is in the run record. Returns
/// what it got when the budget runs out, since `info registers` can wait on
/// a vCPU that is not running.
fn hmp(machine: &Path, commands: &[&str]) -> Result<String> {
    let record = run_record(machine)?;
    let port = record["qmp_port"]
        .as_u64()
        .ok_or("run.json has no qmp_port")? as u16;
    let deadline = Instant::now() + HMP_BUDGET;
    let mut replies = Vec::new();
    let result: Result<()> = (|| {
        let left = || {
            deadline
                .checked_duration_since(Instant::now())
                .ok_or("QEMU monitor budget used up")
        };
        let sock = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse()?, left()?)?;
        let mut reader = BufReader::new(sock.try_clone()?);
        let mut writer = sock;
        let mut call = |request: serde_json::Value| -> Result<serde_json::Value> {
            writeln!(writer, "{request}")?;
            loop {
                let mut line = String::new();
                reader.get_ref().set_read_timeout(Some(left()?))?;
                if reader.read_line(&mut line)? == 0 {
                    return Err("QMP closed".into());
                }
                let reply: serde_json::Value = serde_json::from_str(&line)?;
                if reply.get("return").is_some() || reply.get("error").is_some() {
                    return Ok(reply);
                }
            }
        };
        // The greeting is read by the first call's loop.
        call(serde_json::json!({"execute": "qmp_capabilities"}))?;
        for command in commands {
            let reply = call(serde_json::json!({
                "execute": "human-monitor-command",
                "arguments": {"command-line": command}
            }))?;
            let body = reply
                .get("return")
                .or(reply.get("error"))
                .cloned()
                .unwrap_or_default();
            replies.push(format!(
                "(qemu) {command}\n{}",
                body.as_str().map_or(body.to_string(), String::from)
            ));
        }
        Ok(())
    })();
    if let Err(e) = result {
        replies.push(format!("stopped: {e}"));
    }
    Ok(replies.join("\n"))
}

/// Go prints every goroutine's stack to stderr on SIGQUIT and exits.
/// `install-serve` execs Instigator with stderr on its own server log, so
/// find that file through the process before signalling it.
#[cfg(target_os = "linux")]
fn quit_instigator(out: &Path, pid: u32) -> Result<String> {
    use super::{pause, secs};
    use std::io::{Read, Seek, SeekFrom};
    let log = fs::read_link(format!("/proc/{pid}/fd/2"))?;
    // SAFETY: plain signal delivery to a pid this test started.
    unsafe { libc::kill(pid as i32, libc::SIGQUIT) };
    pause(secs(2));
    let mut f = fs::File::open(&log)?;
    let len = f.seek(SeekFrom::End(0))?;
    f.seek(SeekFrom::Start(len.saturating_sub(1_000_000)))?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail)?;
    fs::write(out.join("instigator-goroutines.txt"), tail)?;
    Ok(format!(
        "sent SIGQUIT to {pid}; last 1 MB of {} saved as instigator-goroutines.txt\n",
        log.display()
    ))
}

#[cfg(not(target_os = "linux"))]
fn quit_instigator(_out: &Path, pid: u32) -> Result<String> {
    Ok(format!(
        "no goroutine dump of {pid}: stderr is found through /proc, which this OS lacks\n"
    ))
}
