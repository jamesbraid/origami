//! The QEMU engine: the product's own CLI creates the machine, attaches the
//! published CHD and starts QEMU; the test drives the serial console.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::checks::{self, Fixtures, Results};
use crate::console::Console;
use crate::data::{Family, System};
use crate::product::{self, Product, RunningGuard};
use crate::{slug, summary, Setup};

/// One system: create, boot, check, halt. Evidence is written whatever happens.
pub fn boot(setup: &Setup, family: &Family, system: &System, name: &str) -> Result<(), String> {
    let out = setup.out.join(slug(name));
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    fs::create_dir_all(&setup.work).map_err(|e| e.to_string())?;
    let product = Product::new(&setup.product, &setup.work);

    let mut results = Results::new();
    let outcome = attempt(&product, setup, family, system, &out, &mut results);
    let error = outcome.as_ref().err().cloned();
    let report = results.json(system, error.as_deref());
    let _ = fs::write(
        out.join("results.json"),
        serde_json::to_string_pretty(&report).unwrap() + "\n",
    );

    let mut text = format!("### {name}: {}\n\n", report["status"].as_str().unwrap());
    if let Some(error) = &error {
        text += &format!("Error: {error}\n\n");
    }
    for (check, ok, detail, _) in &results.checks {
        text += &format!(
            "- {} {check}{}\n",
            if *ok { "pass" } else { "FAIL" },
            if *ok {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }
    summary(&text);

    let mut problems = results.failures();
    if let Some(error) = error {
        problems.insert(0, error);
    }
    if results.checks.is_empty() && problems.is_empty() {
        problems.push("no check ran".into());
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

fn attempt(
    product: &Product,
    setup: &Setup,
    family: &Family,
    system: &System,
    out: &Path,
    results: &mut Results,
) -> Result<(), String> {
    let disk = family.disk.as_ref().unwrap();
    let started = Instant::now();
    let image = product::fetch(&disk.url, &disk.sha256, &setup.disks)?;
    eprintln!("disk ready in {}s", started.elapsed().as_secs());

    let machine = setup
        .work
        .join(slug(&format!("{}-{}", system.family, system.id)));
    let _ = fs::remove_dir_all(&machine);
    let share_dir = setup
        .work
        .join(slug(&format!("{}-{}-share", system.family, system.id)));
    let _ = fs::remove_dir_all(&share_dir);
    fs::create_dir_all(&share_dir).map_err(|e| e.to_string())?;
    let share_data = checks::noise(1 << 20);
    fs::write(share_dir.join("data.bin"), &share_data).map_err(|e| e.to_string())?;
    let cd_data = checks::noise(checks::CD_BYTES);
    let iso = setup
        .work
        .join(slug(&format!("{}-{}-cd.img", system.family, system.id)));
    fs::write(&iso, &cd_data).map_err(|e| e.to_string())?;

    let mut create = vec![
        "create".to_string(),
        machine.display().to_string(),
        "--preset".into(),
        system.preset.clone(),
    ];
    if let Some(memory) = system.memory_per_node {
        create.extend(["--memory-per-node".into(), memory.to_string()]);
    }
    product.ok(&create)?;
    // The overlay keeps the shared published image as it was; the guest's
    // writes land in a diff beside it, which a later trial must not inherit.
    let cleanup = DiffGuard(image.clone());
    cleanup.clear();

    let m = machine.as_os_str();
    product.ok([
        OsStr::new("network-set"),
        m,
        OsStr::new("--mode"),
        OsStr::new("user"),
    ])?;
    product.ok([
        OsStr::new("share"),
        m,
        OsStr::new(checks::SHARE),
        share_dir.as_os_str(),
    ])?;
    product.ok(vec![
        "drive-attach".to_string(),
        machine.display().to_string(),
        iso.display().to_string(),
        "--type".into(),
        "cdrom".into(),
        "--target".into(),
        family.cd_target.to_string(),
    ])?;
    product.ok(vec![
        "drive-attach".to_string(),
        machine.display().to_string(),
        image.display().to_string(),
        "--type".into(),
        "disk".into(),
        "--target".into(),
        family.disk_target.to_string(),
        "--overlay".into(),
    ])?;
    let mut booted = None;
    let mut last_error = String::new();
    for attempt in 1..=2 {
        product.ok(vec![
            "run".to_string(),
            machine.display().to_string(),
            "--display".into(),
            "none".into(),
            "--background".into(),
        ])?;
        let guard = RunningGuard {
            product,
            machine: machine.clone(),
        };
        let boot_started = Instant::now();
        let login = connect(&machine, &out.join("console.log")).and_then(|mut con| {
            con.login(Duration::from_secs(system.login_timeout))?;
            Ok(con)
        });
        let seconds = boot_started.elapsed().as_secs_f64();
        results.checks.clear();
        match login {
            Ok(con) => {
                results.record("login", true, "root shell on the serial console", seconds);
                booted = Some((con, guard));
                break;
            }
            Err(e) => {
                results.record("login", false, &e, seconds);
                last_error = e;
                drop(guard);
                // The next boot starts from the image, not this one's half-written disk.
                cleanup.clear();
                if attempt == 1 {
                    eprintln!("no login; trying one more boot");
                }
            }
        }
    }
    let Some((mut con, guard)) = booted else {
        return Err(last_error);
    };

    let fixtures = Fixtures {
        share_dir: &share_dir,
        share_data: &share_data,
        cd_data: &cd_data,
    };
    let deadline = Instant::now() + Duration::from_secs(1500);
    let checked = checks::run_all(&mut con, family, system, &fixtures, results, deadline);

    let halt_started = Instant::now();
    let halted = halt(&mut con, product, &machine);
    results.record(
        "halt",
        halted,
        "halt -p until QEMU exited",
        halt_started.elapsed().as_secs_f64(),
    );
    drop(guard);
    drop(cleanup);
    checked
}

fn connect(machine: &Path, log: &Path) -> Result<Console, String> {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        match Console::connect(machine, log) {
            Ok(con) => return Ok(con),
            Err(e) if Instant::now() > end => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

/// Shut IRIX down; true if QEMU exited on its own.
fn halt(con: &mut Console, product: &Product, machine: &Path) -> bool {
    if con.line("halt -p").is_err() {
        return false;
    }
    let end = Instant::now() + Duration::from_secs(240);
    while Instant::now() < end {
        if product.is_stopped(machine) {
            return true;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    false
}

/// Deletes the CHD overlay diff beside an image, and the `.new` file it is
/// written through, when dropped.
struct DiffGuard(PathBuf);

impl DiffGuard {
    fn clear(&self) {
        let prefix = format!("{}.diff", self.0.file_name().unwrap().to_string_lossy());
        let Ok(entries) = fs::read_dir(self.0.parent().unwrap()) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

impl Drop for DiffGuard {
    fn drop(&mut self) {
        self.clear();
    }
}
