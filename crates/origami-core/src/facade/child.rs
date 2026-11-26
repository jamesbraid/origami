//! A child process an operation owns. It runs in its own process group
//! (Unix) or job object (Windows), so ending it ends whatever it started,
//! and dropping it never leaves one behind.

use super::operation::Control;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

/// How often a wait looks at the child and at the operation's cancel flag.
const POLL: Duration = Duration::from_millis(25);

pub(crate) struct OwnedChild {
    pub(crate) child: Child,
    done: bool,
    #[cfg(windows)]
    job: Job,
}

#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a job handle is a process-wide kernel handle, usable from any thread.
#[cfg(windows)]
unsafe impl Send for Job {}

impl OwnedChild {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<OwnedChild> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[allow(unused_mut)]
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = match windows_job(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(OwnedChild {
            child,
            done: false,
            #[cfg(windows)]
            job,
        })
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        self.done |= status.is_some();
        Ok(status)
    }

    /// End the child and everything it started, and reap it.
    pub(crate) fn kill(&mut self) {
        if self.done {
            return;
        }
        #[cfg(unix)]
        // SAFETY: SIGKILL to the process group this child leads.
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        #[cfg(windows)]
        // SAFETY: the job handle is open until drop.
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job.0, 1);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.done = true;
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.kill();
        #[cfg(windows)]
        // SAFETY: closes the handle opened in `windows_job`, once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job.0);
        }
    }
}

#[cfg(windows)]
fn windows_job(child: &Child) -> io::Result<Job> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    // SAFETY: plain Win32 calls on a handle made here and the child's own.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(info).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle().cast()) == 0 {
            let error = io::Error::last_os_error();
            CloseHandle(job);
            return Err(error);
        }
        Ok(Job(job))
    }
}

/// What a finished child printed.
pub(crate) struct Finished {
    pub(crate) status: ExitStatus,
    pub(crate) stderr: Vec<u8>,
}

/// Run `command` as an owned child until it exits, feeding `stdin` if it
/// has a pipe for it. None when the operation was cancelled first, with the
/// child gone.
pub(crate) fn run(
    command: &mut Command,
    stdin: Option<Vec<u8>>,
    control: &Control,
) -> io::Result<Option<Finished>> {
    run_watching(command, stdin, control, &mut || {})
}

/// As [`run`], calling `watch` on every poll and once more after the child
/// exits, so the caller sees what it wrote last.
pub(crate) fn run_watching(
    command: &mut Command,
    stdin: Option<Vec<u8>>,
    control: &Control,
    watch: &mut dyn FnMut(),
) -> io::Result<Option<Finished>> {
    let mut owned = OwnedChild::spawn(command)?;
    let feeder = owned
        .child
        .stdin
        .take()
        .zip(stdin)
        .map(|(mut pipe, bytes)| {
            std::thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            })
        });
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        pipe.map(|mut pipe| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                bytes
            })
        })
    };
    let stdout = drain(owned.child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(owned.child.stderr.take().map(|p| Box::new(p) as _));
    let status = loop {
        if control.is_cancelled() {
            owned.kill();
            return Ok(None);
        }
        watch();
        if let Some(status) = owned.try_wait()? {
            break status;
        }
        std::thread::sleep(POLL);
    };
    watch();
    let join = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| {
        handle.map_or_else(Vec::new, |h| h.join().unwrap_or_default())
    };
    drop(feeder);
    join(stdout);
    Ok(Some(Finished {
        status,
        stderr: join(stderr),
    }))
}
