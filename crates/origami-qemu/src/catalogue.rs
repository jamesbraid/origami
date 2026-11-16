//! The machine list QEMU reports in `query-sgi-machines`, translated into
//! the neutral catalogue. What only QEMU's launch reads stays in the
//! offering's engine data, which [`offering_data`] decodes.

use crate::settings::Target;
use crate::ENGINE;
use origami_machine::{Machines, Offering, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::BufReader;
use std::path::Path;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

/// The machine list version this Origami reads. Version 3 names every
/// topology uniquely and gives every offering an id and its settings.
pub const MACHINES_VERSION: u32 = 3;

/// A firmware image that creating a machine's storage reads. Its name is
/// also the init tool's option for that image.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InitInput {
    pub name: String,
    pub kind: String,
    pub required: bool,
}

/// What QEMU's launch reads of an offering, beyond the neutral fields.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OfferingData {
    /// The `-M` value selecting this product, topology and node count.
    #[serde(rename = "machine-options")]
    pub machine_options: String,
    /// The default `-cpu` model.
    #[serde(rename = "default-cpu-model")]
    pub cpu: String,
    #[serde(rename = "init-inputs")]
    pub init_inputs: Vec<InitInput>,
    #[serde(rename = "needs-debug-leds-off", default)]
    pub needs_debug_leds_off: bool,
    /// Launch preparation the machine needs beyond its options, such as
    /// `ip32`; absent from a machine with none.
    #[serde(rename = "launch-recipe", default)]
    pub launch_recipe: Option<String>,
    /// How QEMU binds each of the offering's onboard Ethernets, in the
    /// order of its `networks`.
    #[serde(default)]
    pub nics: Vec<Nic>,
}

/// How QEMU binds one onboard Ethernet.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Nic {
    /// The `-net nic` model that binds it.
    #[serde(rename = "nic-model", default)]
    pub model: Option<String>,
    /// The `-net nic` option naming its address.
    #[serde(rename = "nic-mac-option", default)]
    pub mac_option: Option<String>,
    /// The `-M` property that sets the identity address.
    #[serde(rename = "machine-property", default)]
    pub machine_property: Option<String>,
}

impl OfferingData {
    /// The NIC of the offering's default onboard Ethernet.
    pub fn default_nic(&self, offering: &Offering) -> Option<&Nic> {
        self.nics.get(offering.default_network_index()?)
    }

    /// The `-M` property that sets the machine's identity address.
    pub fn mac_property(&self, offering: &Offering) -> &str {
        self.default_nic(offering)
            .and_then(|nic| nic.machine_property.as_deref())
            .unwrap_or("mac")
    }
}

/// QEMU's data for `offering`.
pub fn offering_data(offering: &Offering) -> Result<OfferingData> {
    let value = offering
        .engine
        .get(ENGINE)
        .ok_or_else(|| format!("{} has no QEMU data", offering.id))?;
    Ok(OfferingData::deserialize(value)?)
}

type Qmp = qapi::Qmp<qapi::Stream<BufReader<ChildStdout>, ChildStdin>>;

/// Ask a short-lived QEMU with no machine for the machine list compiled into it.
pub fn load(qemu: &Path) -> Result<Machines> {
    let machines = ask_machineless(qemu, None, |qmp| {
        let wire = qmp
            .execute(&QuerySgiMachines {})
            .map_err(|e| e.to_string())?;
        translate(wire).map_err(|e| e.to_string())
    })
    .map_err(|error| {
        format!(
            "cannot read the machine list from {}: {error}",
            qemu.display()
        )
    })?;
    check_version(machines, qemu)
}

/// Run `ask` against a short-lived QEMU with no machine, then stop it. A QEMU
/// still silent after `limit` is killed, so `ask` fails instead of hanging.
pub(crate) fn ask_machineless<T>(
    qemu: &Path,
    limit: Option<Duration>,
    ask: impl FnOnce(&mut Qmp) -> std::result::Result<T, String>,
) -> std::result::Result<T, String> {
    let mut child = Command::new(qemu)
        .args([
            "-M",
            "none",
            "-qmp",
            "stdio",
            "-nodefaults",
            "-display",
            "none",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {}: {error}", qemu.display()))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("QEMU's standard streams are not piped".into());
    };
    let watchdog = limit.map(|limit| kill_after(child.id(), limit));
    let reply = without_sigpipe(|| {
        let mut qmp = qapi::Qmp::new(qapi::Stream::new(BufReader::new(stdout), stdin));
        let reply = qmp
            .handshake()
            .map_err(|e| e.to_string())
            .and_then(|_| ask(&mut qmp));
        // Stop QEMU only after the reply is read: it can exit with the end of
        // a long reply still unwritten, so it is killed rather than told to quit.
        drop(qmp);
        reply
    });
    drop(watchdog);
    let _ = child.kill();
    let stderr = child
        .wait_with_output()
        .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
        .unwrap_or_default();
    reply.map_err(|error| format!("{error} {stderr}"))
}

/// Kills process `pid` after `limit` unless the returned sender is dropped first.
fn kill_after(pid: u32, limit: Duration) -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        if rx.recv_timeout(limit) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
            #[cfg(unix)]
            // SAFETY: SIGKILL to a child this process has not yet reaped.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = pid;
        }
    });
    tx
}

/// Run `f` with `SIGPIPE` blocked on this thread. A write to a QEMU that has
/// already exited would otherwise kill a host process that never ignored it,
/// as a cdylib's host does not.
fn without_sigpipe<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(unix)]
    // SAFETY: signal-set calls on this thread's own mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGPIPE);
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
        let out = f();
        // Take a SIGPIPE raised meanwhile, or unblocking would deliver it.
        let mut pending: libc::sigset_t = std::mem::zeroed();
        libc::sigpending(&mut pending);
        if libc::sigismember(&pending, libc::SIGPIPE) == 1 {
            let mut sig = 0;
            libc::sigwait(&set, &mut sig);
        }
        libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut());
        out
    }
    #[cfg(not(unix))]
    f()
}

/// Refuse a machine list of another version: this Origami reads only the
/// version it was built with.
pub fn check_version(machines: Machines, qemu: &Path) -> Result<Machines> {
    if machines.version == MACHINES_VERSION {
        return Ok(machines);
    }
    Err(format!(
        "{} reports machine list version {}, but this Origami reads version {MACHINES_VERSION}; \
         use the QEMU it was built with",
        qemu.display(),
        machines.version
    )
    .into())
}

#[derive(serde::Serialize)]
struct QuerySgiMachines {}

impl qapi::Command for QuerySgiMachines {
    type Ok = Value;
    const NAME: &'static str = "query-sgi-machines";
    const ALLOW_OOB: bool = false;
}

/// The neutral catalogue for the machine list `wire` as QEMU reports it.
/// The neutral types read what they share with QEMU's list directly; this
/// adds the console names, the processor choice and QEMU's own data.
pub fn translate(wire: Value) -> Result<Machines> {
    let mut machines: Machines = serde_json::from_value(wire.clone())?;
    let records = |key: &str| -> Vec<Value> {
        wire.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for (offering, record) in machines.offerings.iter_mut().zip(records("offerings")) {
        let mut data: OfferingData = serde_json::from_value(record.clone())?;
        data.nics = record["networks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|network| Nic {
                model: network["nic-model"].as_str().map(str::to_string),
                mac_option: network["nic-mac-option"].as_str().map(str::to_string),
                machine_property: network["mac"]["machine-property"]
                    .as_str()
                    .map(str::to_string),
            })
            .collect();
        offering.selects_processor = data
            .machine_options
            .split(',')
            .any(|option| option.starts_with("processor="));
        for (console, line) in offering
            .consoles
            .iter_mut()
            .zip(record["consoles"].as_array().into_iter().flatten())
        {
            console.name = line["argument"]
                .as_str()
                .and_then(|argument| argument.strip_prefix("-serial chardev:"))
                .map(str::to_string);
        }
        offering.engines = vec![ENGINE.into()];
        offering
            .engine
            .insert(ENGINE.into(), serde_json::to_value(data)?);
    }
    for (definition, record) in machines
        .setting_definitions
        .iter_mut()
        .zip(records("setting-definitions"))
    {
        let target = record
            .get("target")
            .ok_or("missing field `target` in a setting definition")?;
        let decoded = Target::deserialize(target)?;
        definition.refused = crate::settings::property_and_form(&definition.name, &decoded)
            .err()
            .map(|error| error.to_string());
        definition
            .engine
            .insert(ENGINE.into(), serde_json::json!({ "target": target }));
    }
    Ok(machines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_machines;
    use origami_machine::preset;

    #[test]
    fn the_machine_list_gives_the_mac_property() {
        let machines = test_machines();
        let offer = preset(&machines, "onyx350").unwrap();
        let mut data = offering_data(offer).unwrap();
        assert_eq!(data.mac_property(offer), "mac");
        data.nics[0].machine_property = Some("identity-mac".into());
        assert_eq!(data.mac_property(offer), "identity-mac");
    }

    #[test]
    fn translation_keeps_qemus_launch_data_and_names_the_neutral_parts() {
        let machines = test_machines();
        let fuel = preset(&machines, "fuel-1").unwrap();
        assert_eq!(fuel.engines, [ENGINE]);
        let data = offering_data(fuel).unwrap();
        assert_eq!(data.machine_options, "fuel,topology=fuel,nodes=1");
        assert_eq!(data.nics[0].model.as_deref(), Some("sgi-ioc3-eth"));
        assert_eq!(fuel.consoles[0].name.as_deref(), Some("l1"));
        assert!(!fuel.selects_processor);
        let selected = machines
            .offerings
            .iter()
            .find(|o| o.id == "origin200.2.r12000-270mhz-4mb")
            .unwrap();
        assert!(selected.selects_processor);
        // A setting no engine serves is refused in the neutral catalogue.
        let wire = serde_json::json!({
            "version": MACHINES_VERSION,
            "offerings": [],
            "setting-definitions": [{
                "name": "lines",
                "kind": "boolean",
                "target": { "kind": "serial-lines" }
            }]
        });
        let machines = translate(wire).unwrap();
        let refused = machines.setting_definitions[0].refused.as_deref().unwrap();
        assert!(refused.contains("backs serial lines"), "{refused}");
    }

    #[cfg(unix)]
    #[test]
    fn the_catalogue_is_read_from_qemu_over_qmp() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::test_support::fake_runtime(root.path());
        let machines = load(&runtime.join("qemu-system-mips64")).unwrap();
        assert_eq!(machines.version, MACHINES_VERSION);
        assert_eq!(machines.offerings.len(), test_machines().offerings.len());
        let other = Machines {
            version: MACHINES_VERSION + 1,
            ..machines
        };
        let error = check_version(other, Path::new("qemu"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("reads version 3"), "{error}");
    }
}
