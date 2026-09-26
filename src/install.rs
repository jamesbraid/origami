use crate::{resolve, tcp_endpoint, MachineFile, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    let manifest = InstallMedia { format: 1, media };
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
    Ok(json!({
        "server_ip": "10.98.0.2",
        "netmask": "10.98.0.0/24",
        "cache_dir": dir.join("install/cache"),
        "clients": [{ "name": "sgi", "mac": mac, "ip": "10.98.0.65" }],
        "services": {
            "bootp": true, "tftp": { "port_range": [2048, 32767] }, "rsh": true
        },
        "install_scripts": [{
            "name": "mipspro",
            "install": [
                "c_fe.sw.c", "c_dev.sw.c", "compiler_dev.sw.base",
                "compiler_dev.sw.ld", "dev.sw.lib"
            ]
        }],
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
