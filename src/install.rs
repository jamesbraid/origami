use crate::{resolve, tcp_endpoint, MachineFile, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
#[cfg(windows)]
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallMedia {
    pub format: u32,
    pub media: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addons: Vec<InstallAddon>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallAddon {
    pub name: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dist: Option<String>,
    pub install: Vec<String>,
}

struct Layer {
    set: &'static str,
    name: &'static str,
    path: &'static str,
    boot: bool,
    base: Option<&'static str>,
    dist: Option<&'static str>,
}

const LAYERS: &[Layer] = &[
    Layer {
        set: "6.5.30",
        name: "overlays1",
        path: "media/6.5.30/overlays1.image",
        boot: true,
        base: None,
        dist: None,
    },
    Layer {
        set: "6.5.30",
        name: "overlays2",
        path: "media/6.5.30/overlays2.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "6.5.30",
        name: "overlays3",
        path: "media/6.5.30/overlays3.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "foundations",
        name: "foundation1",
        path: "media/6.5-base/foundation1.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "foundations",
        name: "foundation2",
        path: "media/6.5-base/foundation2.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "foundations",
        name: "onc3-nfs",
        path: "media/6.5-base/nfs.image",
        boot: false,
        base: None,
        dist: Some("dist6.5"),
    },
    Layer {
        set: "development",
        name: "devlibs",
        path: "media/6.5-base/devlibs.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "development",
        name: "devfoundation",
        path: "media/6.5-base/devfoundation.image",
        boot: false,
        base: None,
        dist: Some("dist/dist6.5"),
    },
    Layer {
        set: "development",
        name: "mipspro744update",
        path: "media/mipspro/7.4.4/mipspro744update.tar.gz",
        boot: false,
        base: Some("MIPSPro7.4.4"),
        dist: Some("."),
    },
    Layer {
        set: "development",
        name: "mipspro_c",
        path: "media/mipspro/7.4.4/mipspro_c.tar.gz",
        boot: false,
        base: Some("mipspro_c"),
        dist: Some("dist"),
    },
    Layer {
        set: "applications",
        name: "applications",
        path: "media/6.5.30/applications.image",
        boot: false,
        base: None,
        dist: None,
    },
    Layer {
        set: "complementary",
        name: "complementary",
        path: "media/6.5.30/complementary.image",
        boot: false,
        base: None,
        dist: None,
    },
];

const MIPSPRO_INSTALL: &[&str] = &[
    "c_fe.sw.c",
    "c_dev.sw.c",
    "compiler_dev.sw.base",
    "compiler_dev.sw.ld",
    "dev.sw.lib",
];

const SETS: &[&str] = &[
    "6.5.30",
    "foundations",
    "development",
    "applications",
    "complementary",
];

pub fn init(dir: &Path, media_root: &Path, mac: &str, file: &mut MachineFile) -> Result<PathBuf> {
    let root = media_root.canonicalize()?;
    let install_dir = dir.join("install");
    let manifest_path = install_dir.join("media.toml");
    if manifest_path.exists() {
        return Err(format!(
            "install media configuration already exists: {}",
            manifest_path.display()
        )
        .into());
    }
    let media = LAYERS
        .iter()
        .map(|layer| {
            (
                layer.name.into(),
                root.join(layer.path).display().to_string(),
            )
        })
        .collect();
    let manifest = InstallMedia {
        format: 1,
        media,
        addons: vec![],
    };
    let catalog = crate::catalogue()?;
    let old_network = file.network.clone();
    #[cfg(windows)]
    let endpoint = {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        format!("tcp:{}", listener.local_addr()?)
    };
    #[cfg(not(windows))]
    let endpoint = "install/network.sock".to_string();
    file.network = crate::Network {
        mode: "private".into(),
        endpoint: Some(endpoint),
        mac: Some(mac.into()),
    };
    if let Err(error) = crate::validate(&catalog, dir, file) {
        file.network = old_network;
        return Err(error);
    }
    fs::create_dir_all(&install_dir)?;
    fs::write(&manifest_path, toml::to_string_pretty(&manifest)?)?;
    fs::write(dir.join("machine.toml"), toml::to_string_pretty(file)?)?;
    Ok(manifest_path)
}

fn valid_addon_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_install_selection(item: &str) -> bool {
    !item.is_empty()
        && item
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

pub fn add_addon(
    dir: &Path,
    name: &str,
    source: &Path,
    base: Option<&str>,
    dist: Option<&str>,
    install: &[String],
) -> Result<PathBuf> {
    if !valid_addon_name(name) {
        return Err("add-on name must use letters, digits, hyphens, or underscores".into());
    }
    if install.is_empty() || install.iter().any(|item| !valid_install_selection(item)) {
        return Err("add-on needs one or more package names using letters, digits, dots, underscores, hyphens, or plus signs".into());
    }
    let source = source.canonicalize()?;
    if !source.is_file() && !source.is_dir() {
        return Err(format!(
            "add-on source is not a file or directory: {}",
            source.display()
        )
        .into());
    }
    let mut media = read_media(dir)?;
    if media.addons.iter().any(|addon| addon.name == name) {
        return Err(format!("add-on already configured: {name}").into());
    }
    media.addons.push(InstallAddon {
        name: name.into(),
        source: source.display().to_string(),
        base: base.map(str::to_owned),
        dist: dist.map(str::to_owned),
        install: install.to_vec(),
    });
    let path = dir.join("install/media.toml");
    fs::write(&path, toml::to_string_pretty(&media)?)?;
    Ok(path)
}

pub fn read_media(dir: &Path) -> Result<InstallMedia> {
    let file: InstallMedia = toml::from_str(&fs::read_to_string(dir.join("install/media.toml"))?)?;
    if file.format != 1 {
        return Err(format!("unsupported install media format {}", file.format).into());
    }
    Ok(file)
}

pub fn config(dir: &Path, file: &MachineFile, media: &InstallMedia) -> Result<Value> {
    if file.network.mode != "private" {
        return Err("installation requires a private network".into());
    }
    let mac = file
        .network
        .mac
        .as_deref()
        .ok_or("private network needs mac")?;
    let mut sets = Vec::new();
    for name in SETS {
        let mut layers = Vec::new();
        for layer in LAYERS.iter().filter(|layer| layer.set == *name) {
            let path = media
                .media
                .get(layer.name)
                .ok_or_else(|| format!("missing install media: {}", layer.name))?;
            let source = resolve(dir, path);
            if !source.is_file() && !source.is_dir() {
                return Err(
                    format!("missing install media {}: {}", layer.name, source.display()).into(),
                );
            }
            let mut entry = json!({ "name": layer.name, "source": source });
            if layer.boot {
                entry["boot"] = json!(true);
            }
            if let Some(base) = layer.base {
                entry["base"] = json!(base);
            }
            if let Some(dist) = layer.dist {
                entry["dist"] = json!(dist);
            }
            layers.push(entry);
        }
        let mut set = json!({ "name": name, "layers": layers });
        if *name == "development" {
            set["collisions"] = json!({ "development/dist/inst.README": "mipspro744update" });
        }
        sets.push(set);
    }
    let mut scripts = vec![json!({ "name": "mipspro", "install": MIPSPRO_INSTALL })];
    let mut addon_names = HashSet::new();
    for addon in &media.addons {
        if !addon_names.insert(&addon.name) {
            return Err(format!("duplicate add-on name: {}", addon.name).into());
        }
        if !valid_addon_name(&addon.name) {
            return Err(format!("invalid add-on name: {}", addon.name).into());
        }
        if addon.install.is_empty()
            || addon
                .install
                .iter()
                .any(|item| !valid_install_selection(item))
        {
            return Err(format!("add-on {} has invalid install selections", addon.name).into());
        }
        let source = resolve(dir, &addon.source);
        if !source.is_file() && !source.is_dir() {
            return Err(format!("missing add-on {}: {}", addon.name, source.display()).into());
        }
        let mut layer = json!({ "name": addon.name, "source": source });
        if let Some(base) = &addon.base {
            layer["base"] = json!(base);
        }
        if let Some(dist) = &addon.dist {
            layer["dist"] = json!(dist);
        }
        sets.push(json!({
            "name": format!("addon-{}", addon.name),
            "layers": [layer],
        }));
        let mut selected: Vec<&str> = MIPSPRO_INSTALL.to_vec();
        selected.extend(addon.install.iter().map(String::as_str));
        scripts.push(json!({
            "name": format!("addon-{}", addon.name),
            "install": selected,
        }));
    }
    Ok(json!({
        "server_ip": "10.98.0.2",
        "netmask": "10.98.0.0/24",
        "cache_dir": dir.join("install/cache"),
        "clients": [{ "name": "sgi", "mac": mac, "ip": "10.98.0.65" }],
        "services": {
            "bootp": true, "tftp": { "port_range": [2048, 32767] }, "rsh": true
        },
        "install_scripts": scripts,
        "install_sets": sets
    }))
}

pub fn serve(dir: &Path, file: &MachineFile) -> Result<ExitStatus> {
    let media = read_media(dir)?;
    let document = config(dir, file, &media)?;
    let install_dir = dir.join("install");
    fs::create_dir_all(install_dir.join("cache"))?;
    let config_path = install_dir.join("instigator.json");
    fs::write(&config_path, serde_json::to_vec_pretty(&document)?)?;
    let endpoint = file
        .network
        .endpoint
        .as_deref()
        .ok_or("private network needs endpoint")?;
    let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "instigator.exe"
    } else {
        "instigator"
    });
    if !executable.is_file() {
        return Err(format!("packaged Instigator missing: {}", executable.display()).into());
    }
    let mut command = Command::new(executable);
    command.arg("serve");
    if let Some(address) = tcp_endpoint(endpoint)? {
        command.arg("--network-tcp").arg(address.to_string());
    } else {
        command.arg("--network-socket").arg(resolve(dir, endpoint));
    }
    command.arg(config_path).current_dir(dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        Ok(command.status()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addon_script_selects_mipspro_and_local_package() {
        use crate::{Firmware, Machine, Network};
        use std::time::{SystemTime, UNIX_EPOCH};

        let dir = std::env::temp_dir().join(format!(
            "sgi-addon-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("tablet.tardist");
        fs::write(&source, b"synthetic source").unwrap();
        let media = InstallMedia {
            format: 1,
            media: LAYERS
                .iter()
                .map(|layer| (layer.name.into(), source.display().to_string()))
                .collect(),
            addons: vec![InstallAddon {
                name: "tablet".into(),
                source: source.display().to_string(),
                base: Some("tablet-disc".into()),
                dist: Some("dist".into()),
                install: vec!["tablet.sw.helper".into()],
            }],
        };
        let file = MachineFile {
            format: 1,
            machine: Machine {
                model: "origin200".into(),
                nodes: 1,
                cpus_per_node: 1,
                memory_per_node: "256MiB".into(),
                graphics: "rad4".into(),
            },
            firmware: Firmware {
                image: "prom.bin".into(),
            },
            identity: None,
            network: Network {
                mode: "private".into(),
                endpoint: Some("install/network.sock".into()),
                mac: Some("08:00:69:12:34:56".into()),
            },
            drive: vec![],
        };
        let document = config(&dir, &file, &media).unwrap();
        let addon_set = &document["install_sets"][SETS.len()];
        assert_eq!(addon_set["name"], "addon-tablet");
        assert_eq!(
            addon_set["layers"][0]["source"],
            source.display().to_string()
        );
        assert_eq!(addon_set["layers"][0]["base"], "tablet-disc");
        let script = &document["install_scripts"][1];
        assert_eq!(script["name"], "addon-tablet");
        assert!(script["install"]
            .as_array()
            .unwrap()
            .contains(&json!("c_fe.sw.c")));
        assert!(script["install"]
            .as_array()
            .unwrap()
            .contains(&json!("tablet.sw.helper")));
        let mut invalid = media.clone();
        invalid.addons[0].install = vec!["tablet.sw.helper\nquit".into()];
        assert!(config(&dir, &file, &invalid).is_err());
        invalid.addons[0].install = vec!["tablet.sw.helper".into()];
        invalid.addons.push(invalid.addons[0].clone());
        assert!(config(&dir, &file, &invalid).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn standard_profile_contains_mipspro_media() {
        assert_eq!(
            LAYERS
                .iter()
                .filter(|layer| layer.set == "development" && layer.name.starts_with("mipspro"))
                .count(),
            2
        );
        assert!(LAYERS
            .iter()
            .any(|layer| layer.name == "overlays1" && layer.boot));
        assert_eq!(SETS.len(), 5);
    }
}
