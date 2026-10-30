use crate::{control, resolve, tcp_endpoint, MachineFile, Machines, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallMedia {
    pub format: u32,
    #[serde(default = "legacy_profile")]
    pub profile: String,
    /// The IRIX release whose recipe built this list. A media.toml from before
    /// releases were selectable has none and means 6.5.30.
    #[serde(default = "default_release")]
    pub release: String,
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

/// Where Instigator's shipped configurations and Origami's media lists keep
/// the public disc images. A local --media-root mirrors this layout, with
/// each .iso file stored as .image.
const BASE_URL: &str = "https://origami-dist.irix.fans/irix/";

/// An Instigator add-on file: extra software installed after IRIX.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddonFile {
    name: String,
    layers: Vec<AddonLayer>,
    install: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddonLayer {
    name: String,
    source: String,
    base: Option<String>,
    dist: Option<String>,
}

/// Add-ons whose archives a --media-root does not hold.
const REMOTE_ONLY_ADDONS: &[&str] = &["rad4"];

/// One disc image or directory of an install set.
struct Layer {
    name: String,
    /// The public URL Instigator's configuration names.
    url: String,
}

impl Layer {
    /// Where a --media-root holds this layer.
    fn local_path(&self) -> Result<String> {
        let path = self
            .url
            .strip_prefix(BASE_URL)
            .ok_or_else(|| format!("layer {} is not under {BASE_URL}", self.name))?;
        Ok(match path.strip_suffix(".iso") {
            Some(stem) => format!("{stem}.image"),
            None => path.into(),
        })
    }
}

/// An Instigator configuration for one release and profile, as shipped,
/// with the parts Origami reads picked out.
struct InstallRecipe {
    document: Value,
    layers: Vec<Layer>,
}

/// A media.toml from before install profiles has none. It installed
/// MIPSpro with IRIX, which no profile does now, so recipe() rejects it.
fn legacy_profile() -> String {
    "legacy-development".into()
}

/// The release a media.toml without one was built for, and the one
/// install-init uses unless told otherwise.
pub const DEFAULT_RELEASE: &str = "6.5.30";

fn default_release() -> String {
    DEFAULT_RELEASE.into()
}

/// Instigator's shipped configuration, embedded so the binary needs no
/// checkout.
fn release_config(release: &str, profile: &str) -> Result<&'static str> {
    match (release, profile) {
        ("6.5.30", "minimal") => Ok(include_str!(
            "../instigator/configs/irix-6.5.30-minimal.yaml"
        )),
        ("6.5.30", "standard") => Ok(include_str!(
            "../instigator/configs/irix-6.5.30-standard.yaml"
        )),
        ("6.5.22", "standard") => Ok(include_str!(
            "../instigator/configs/irix-6.5.22-standard.yaml"
        )),
        ("6.5.22", _) => Err(format!("IRIX {release} has no {profile} install profile").into()),
        _ => Err(format!("unknown IRIX release: {release}").into()),
    }
}

/// An add-on Instigator ships.
fn addon_file(name: &str) -> Option<&'static str> {
    match name {
        "mipspro" => Some(include_str!("../instigator/configs/addons/mipspro.yaml")),
        "rad4" => Some(include_str!("../instigator/configs/addons/rad4.yaml")),
        _ => None,
    }
}

fn recipe(release: &str, profile: &str) -> Result<InstallRecipe> {
    // A media.toml from an earlier release can still name base or desktop.
    let selection_profile = match profile {
        "minimal" | "base" => "minimal",
        "standard" | "desktop" => "standard",
        "development" | "legacy-development" => {
            return Err(format!(
                "install profile {profile} was removed. Remove install/media.toml, run install-init with --profile standard, then add MIPSpro with install-addon DIR --name mipspro"
            )
            .into())
        }
        _ => return Err(format!("unknown install profile: {profile}").into()),
    };
    if !["6.5.22", "6.5.30"].contains(&release) {
        return Err(format!("unknown IRIX release: {release}").into());
    }
    let document: Value = serde_saphyr::from_str(release_config(release, selection_profile)?)
        .map_err(|error| format!("Instigator configuration for IRIX {release}: {error}"))?;
    let mut layers = Vec::new();
    for set in document["install_sets"].as_array().into_iter().flatten() {
        for layer in set["layers"].as_array().into_iter().flatten() {
            layers.push(Layer {
                name: layer["name"].as_str().unwrap_or_default().into(),
                url: layer["source"].as_str().unwrap_or_default().into(),
            });
        }
    }
    let has_script = document["install_scripts"]
        .as_array()
        .is_some_and(|scripts| {
            scripts
                .iter()
                .any(|script| script["name"] == STANDARD_INSTALL_SCRIPT)
        });
    if !has_script {
        return Err(format!(
            "Instigator configuration for IRIX {release} has no {STANDARD_INSTALL_SCRIPT} script"
        )
        .into());
    }
    Ok(InstallRecipe { document, layers })
}

/// Build an add-on from the configuration Instigator ships, from a local
/// media root or the public archive.
fn named_addon(name: &str, media_root: Option<&Path>) -> Result<InstallAddon> {
    let text = addon_file(name).ok_or_else(|| {
        format!("no known add-on {name}; give its --source and --install selections")
    })?;
    let file: AddonFile = serde_saphyr::from_str(text)
        .map_err(|error| format!("Instigator add-on {name}: {error}"))?;
    let layer = file
        .layers
        .into_iter()
        .next()
        .ok_or_else(|| format!("Instigator add-on {name} has no layer"))?;
    // The generated add-on file names its one layer after the add-on.
    if layer.name != file.name {
        return Err(format!("Instigator add-on {name} names its layer {}", layer.name).into());
    }
    let source = match media_root {
        Some(_) if REMOTE_ONLY_ADDONS.contains(&name) => {
            return Err(format!("the {name} add-on has no local media; omit --media-root").into())
        }
        Some(root) => {
            let path = layer
                .source
                .strip_prefix(BASE_URL)
                .ok_or_else(|| format!("add-on {name} is not under {BASE_URL}"))?;
            root.canonicalize()?.join(path).display().to_string()
        }
        None => layer.source,
    };
    Ok(InstallAddon {
        name: file.name,
        source,
        base: layer.base,
        dist: layer.dist,
        install: file.install,
    })
}

/// The script the user guide names. It installs the profile, then every
/// configured add-on in one second pass, so a machine whose graphics need a
/// driver add-on, such as RAD4, gets it from the same entry point.
const STANDARD_INSTALL_SCRIPT: &str = "irix";

/// Commands every install script runs in the installed system after its
/// last go. proclaim, IRIX's DHCP client, runs at boot when this flag is on.
/// IRIX 6.5.30 ships it on, but proclaim has been seen to turn it off when
/// it boots with a lease and gets no answer, as on the installation network.
/// The PROM waits at its System Maintenance Menu unless AutoLoad is set,
/// and installation leaves it unset; the machine keeps the setting in NVRAM.
const POST_INSTALL: &[&str] = &["chkconfig -f autoconfig_ipaddress on", "nvram AutoLoad Yes"];
/// Instigator's post-install file prints this once every command succeeded.
const POST_INSTALL_DONE: &[u8] = b"instigator: post-install commands finished";
/// inst prints this for a failing command and skips the rest of the file.
const COMMAND_FAILED: &[u8] = b"(command failed)";

/// Whether the machine has a RAD4, whose driver it needs to reach a
/// graphical login.
pub fn needs_rad4_driver(file: &MachineFile) -> bool {
    file.pipe.iter().any(|pipe| pipe.product == "rad4")
}

pub fn init_profile(
    machines: &Machines,
    dir: &Path,
    media_root: &Path,
    mac: Option<&str>,
    file: &mut MachineFile,
    release: &str,
    profile: &str,
) -> Result<PathBuf> {
    let root = media_root.canonicalize()?;
    init_sources(machines, dir, mac, file, Some(&root), release, profile)
}

pub fn init_remote_profile(
    machines: &Machines,
    dir: &Path,
    mac: Option<&str>,
    file: &mut MachineFile,
    release: &str,
    profile: &str,
) -> Result<PathBuf> {
    init_sources(machines, dir, mac, file, None, release, profile)
}

fn init_sources(
    machines: &Machines,
    dir: &Path,
    mac: Option<&str>,
    file: &mut MachineFile,
    root: Option<&Path>,
    release: &str,
    profile: &str,
) -> Result<PathBuf> {
    let recipe = recipe(release, profile)?;
    let install_dir = dir.join("install");
    let manifest_path = install_dir.join("media.toml");
    if manifest_path.exists() {
        return Err(format!(
            "install media configuration already exists: {}",
            manifest_path.display()
        )
        .into());
    }
    let mut media = BTreeMap::new();
    for layer in &recipe.layers {
        let source = match root {
            Some(root) => root.join(layer.local_path()?).display().to_string(),
            None => layer.url.clone(),
        };
        media.insert(layer.name.clone(), source);
    }
    let manifest = InstallMedia {
        format: 1,
        profile: profile.into(),
        release: release.into(),
        media,
        // A RAD4 machine needs the driver to reach a graphical login.
        addons: if needs_rad4_driver(file) {
            vec![named_addon("rad4", None)?]
        } else {
            vec![]
        },
    };
    let old_network = file.network.clone();
    let old_identity = file.identity.clone();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("tcp:{}", listener.local_addr()?);
    file.network = crate::Network {
        mode: "private".into(),
        endpoint: Some(endpoint),
        interface: None,
        forward: old_network.forward.clone(),
        share_address: None,
    };
    let assigned = match crate::ensure_machine_mac(machines, dir, file, mac)
        .and_then(|assigned| crate::validate(machines, dir, file).map(|_| assigned))
    {
        Ok(assigned) => assigned,
        Err(error) => {
            file.network = old_network;
            file.identity = old_identity;
            return Err(error);
        }
    };
    fs::create_dir_all(&install_dir)?;
    fs::write(&manifest_path, toml::to_string_pretty(&manifest)?)?;
    fs::write(dir.join("machine.toml"), toml::to_string_pretty(file)?)?;
    if let Some(mac) = assigned {
        println!("assigned MAC {mac}");
    }
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

/// Configure an add-on Origami knows by name: mipspro, from the public
/// archive or a local media root, or rad4, from the public archive.
pub fn add_known_addon(dir: &Path, name: &str, media_root: Option<&Path>) -> Result<PathBuf> {
    let addon = named_addon(name, media_root)?;
    add_addon(
        dir,
        &addon.name,
        &addon.source,
        addon.base.as_deref(),
        addon.dist.as_deref(),
        &addon.install,
    )
}

pub fn add_addon(
    dir: &Path,
    name: &str,
    source: &str,
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
    let source = if source.contains("://") {
        media_source(dir, source)?
    } else {
        let source = Path::new(source).canonicalize()?;
        if !source.is_file() && !source.is_dir() {
            return Err(format!(
                "add-on source is not a file or directory: {}",
                source.display()
            )
            .into());
        }
        source.display().to_string()
    };
    let mut media = read_media(dir)?;
    if media.addons.iter().any(|addon| addon.name == name) {
        return Err(format!("add-on already configured: {name}").into());
    }
    media.addons.push(InstallAddon {
        name: name.into(),
        source,
        base: base.map(str::to_owned),
        dist: dist.map(str::to_owned),
        install: install.to_vec(),
    });
    let path = dir.join("install/media.toml");
    fs::write(&path, toml::to_string_pretty(&media)?)?;
    Ok(path)
}

pub fn read_media(dir: &Path) -> Result<InstallMedia> {
    let path = dir.join("install/media.toml");
    let text = fs::read_to_string(&path).map_err(|error| {
        format!("cannot read {}: {error}. Configure installation first with origami install-init {} --mac MAC --profile standard", path.display(), dir.display())
    })?;
    let file: InstallMedia = toml::from_str(&text)
        .map_err(|error| format!("invalid install configuration {}: {error}", path.display()))?;
    if file.format != 1 {
        return Err(format!("unsupported install media format {}", file.format).into());
    }
    Ok(file)
}

fn media_source(dir: &Path, value: &str) -> Result<String> {
    if value.contains("://") {
        crate::assets::validate_https_url(value)?;
        return Ok(value.into());
    }
    let path = resolve(dir, value);
    if !path.is_file() && !path.is_dir() {
        return Err(format!("missing install media: {}", path.display()).into());
    }
    Ok(path.display().to_string())
}

/// The configuration handed to Instigator and the add-on files it names.
pub struct InstallConfig {
    pub document: Value,
    /// Instigator reads each add-on from a file, so these are written before
    /// it runs.
    pub addon_files: Vec<(PathBuf, String)>,
}

impl InstallConfig {
    pub fn write_addon_files(&self) -> Result<()> {
        for (path, text) in &self.addon_files {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, text)?;
        }
        Ok(())
    }
}

/// Instigator's shipped configuration for the machine's release and
/// profile, with this machine's network, media and add-ons filled in.
pub fn config(dir: &Path, file: &MachineFile, media: &InstallMedia) -> Result<InstallConfig> {
    let recipe = recipe(&media.release, &media.profile)?;
    if file.network.mode != "private" {
        return Err("installation requires a private network".into());
    }
    let mac = file
        .identity
        .as_ref()
        .map(|identity| identity.mac.as_str())
        .ok_or("installation needs the machine's MAC")?;
    let mut document = recipe.document;
    for set in document["install_sets"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        for layer in set["layers"].as_array_mut().into_iter().flatten() {
            let name = layer["name"].as_str().unwrap_or_default();
            let path = media
                .media
                .get(name)
                .ok_or_else(|| format!("missing install media: {name}"))?;
            layer["source"] = json!(media_source(dir, path)?);
        }
    }
    let mut addon_names = HashSet::new();
    let mut addon_files = Vec::new();
    let mut addon_paths = Vec::new();
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
        let source = media_source(dir, &addon.source)?;
        let mut layer = json!({ "name": addon.name, "source": source });
        if let Some(base) = &addon.base {
            layer["base"] = json!(base);
        }
        if let Some(dist) = &addon.dist {
            layer["dist"] = json!(dist);
        }
        let text = serde_saphyr::to_string(&json!({
            "name": addon.name,
            "layers": [layer],
            "install": addon.install,
        }))
        .map_err(|error| format!("add-on {}: {error}", addon.name))?;
        // Instigator installs an add-on in a second pass after IRIX, so its
        // files go through the symbolic links IRIX's own products make, such
        // as /var/sysgen/boot, instead of replacing them with directories.
        let path =
            std::path::absolute(dir.join("install/addons"))?.join(format!("{}.yaml", addon.name));
        addon_paths.push(path.display().to_string());
        addon_files.push((path, text));
    }
    if !addon_paths.is_empty() {
        document["addons"] = json!(addon_paths);
    }
    for script in document["install_scripts"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        script["post_install"] = json!(POST_INSTALL);
    }
    let mut client = json!({ "name": "sgi", "mac": mac, "ip": "10.98.0.65" });
    // The miniroot offers the BOOTP name as its default hostname, and the
    // installed system keeps it, so the machine is named after its directory
    // from first boot. Instigator allows 40 characters.
    if let Some(name) = crate::runtime::dhcp_hostname(dir).filter(|name| name.len() <= 40) {
        client["hostname"] = json!(name);
    }
    document["server_ip"] = json!("10.98.0.2");
    document["netmask"] = json!("10.98.0.0/24");
    document["cache_dir"] = json!(dir.join("install/cache"));
    document["clients"] = json!([client]);
    Ok(InstallConfig {
        document,
        addon_files,
    })
}

pub fn instigator_path() -> Result<PathBuf> {
    let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "instigator.exe"
    } else {
        "instigator"
    });
    if !executable.is_file() {
        return Err(format!("packaged Instigator missing: {}", executable.display()).into());
    }
    Ok(executable)
}

/// Runs Instigator's check on the generated configuration. Remote media are
/// only probed for reachability unless `full` asks Instigator to read and
/// assemble them as install-serve does.
pub fn check(dir: &Path, file: &MachineFile, full: bool) -> Result<()> {
    let media = read_media(dir)?;
    let config = config(dir, file, &media)?;
    config.write_addon_files()?;
    fs::create_dir_all(dir.join("install/cache"))?;
    let mut args = vec!["check"];
    if full {
        args.push("--full");
    }
    args.push("-");
    let mut child = Command::new(instigator_path()?)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("Instigator check has no stdin")?
        .write_all(&serde_json::to_vec(&config.document)?)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "Instigator rejected install media: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

pub fn serve(dir: &Path, file: &MachineFile) -> Result<ExitStatus> {
    let media = read_media(dir)?;
    let config = config(dir, file, &media)?;
    let install_dir = dir.join("install");
    fs::create_dir_all(install_dir.join("cache"))?;
    config.write_addon_files()?;
    let config_path = install_dir.join("instigator.json");
    fs::write(&config_path, serde_json::to_vec_pretty(&config.document)?)?;
    let endpoint = file
        .network
        .endpoint
        .as_deref()
        .ok_or("private network needs endpoint")?;
    let mut command = Command::new(instigator_path()?);
    command.arg("serve");
    if let Some(address) = tcp_endpoint(endpoint)? {
        command.arg("--network-tcp").arg(address.to_string());
    } else {
        command.arg("--network-socket").arg(resolve(dir, endpoint));
    }
    let capture_dir = install_dir.join(format!(
        "instigator-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    // Each server attempt keeps its own output and native timing records.
    fs::create_dir(&capture_dir)?;
    let log_path = capture_dir.join("server.log");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let log = options.open(&log_path)?;
    eprintln!("Instigator log: {}", log_path.display());
    eprintln!("Instigator timing capture: {}", capture_dir.display());
    command
        .arg("--capture-dir")
        .arg(&capture_dir)
        .arg(config_path)
        .current_dir(dir)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
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

const INSTALL_PROMPT: &[u8] = b"Inst> ";
const SHELL_PROMPT: &[u8] = b"# ";
const RESTART_PROMPT: &[u8] = b"Restart? { (y)es, (n)o, (sh)ell, (h)elp }: ";
const SERIAL_LOGIN_PROMPT_SUFFIX: &[u8] = b" console login: ";
const TERMINAL_PROMPT: &[u8] = b"TERM = (vt100) ";
// IRIX's startup autoconfig prints this when it built a kernel that only a
// further restart loads.
const STARTUP_REBUILD: &str = "Reboot to start using the reconfigured kernel.";

struct InstallConsole {
    stream: TcpStream,
}

impl InstallConsole {
    fn connect(dir: &Path) -> Result<Self> {
        let record = control::read(dir).map_err(|error| {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
            {
                "install commands require a background run; start with origami run DIR --background"
                    .into()
            } else {
                error
            }
        })?;
        control::verified_qmp(&record)?;
        let address = SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, record.console_port);
        let stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { stream })
    }

    fn wait_for(&mut self, prompt: &[u8], timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut chunk = [0u8; 4096];
        while Instant::now() < deadline {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("guest serial console closed".into()),
                Ok(count) => {
                    io::stdout().write_all(&chunk[..count])?;
                    io::stdout().flush()?;
                    output.extend_from_slice(&chunk[..count]);
                    if output.len() > 65536 {
                        output.drain(..output.len() - 65536);
                    }
                    if output.ends_with(prompt) {
                        return Ok(String::from_utf8_lossy(&output).into_owned());
                    }
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(format!(
            "guest did not reach {} within {} seconds; machine remains running",
            String::from_utf8_lossy(prompt),
            timeout.as_secs()
        )
        .into())
    }

    fn command(&mut self, command: &str, prompt: &[u8], timeout: Duration) -> Result<String> {
        self.stream.write_all(command.as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.wait_for(prompt, timeout)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallPhase {
    ReadingSources,
    Transferring,
    Checking,
    Finalizing,
}

struct InstallProgress {
    phase: InstallPhase,
    passes: usize,
    pass: usize,
    pending: Vec<u8>,
    menu_pending: Vec<u8>,
    menu_answered: bool,
    conflict_pending: Vec<u8>,
    conflict_seen: bool,
    conflict_prompt_pending: Vec<u8>,
    post_install_pending: Vec<u8>,
    post_install_done: bool,
}

impl InstallProgress {
    /// Follow `passes` runs of go from one command file. inst prints no
    /// Inst> prompt between them: the next pass starts reading its
    /// distribution as soon as the previous one has calculated sizes.
    fn new(passes: usize) -> Self {
        Self {
            phase: InstallPhase::ReadingSources,
            passes,
            pass: 1,
            pending: Vec::new(),
            menu_pending: Vec::new(),
            menu_answered: false,
            conflict_pending: Vec::new(),
            conflict_seen: false,
            conflict_prompt_pending: Vec::new(),
            post_install_pending: Vec::new(),
            post_install_done: false,
        }
    }

    /// Watch for the post-install result. A failed command stops the
    /// command file, so the done line never follows it.
    fn observe_post_install(&mut self, bytes: &[u8]) -> Result<()> {
        if self.post_install_done {
            return Ok(());
        }
        self.post_install_pending.extend_from_slice(bytes);
        let contains = |pending: &[u8], marker: &[u8]| {
            pending.windows(marker.len()).any(|window| window == marker)
        };
        if contains(&self.post_install_pending, COMMAND_FAILED) {
            return Err(
                "an inst command failed; post-install commands did not finish; guest remains running"
                    .into(),
            );
        }
        if contains(&self.post_install_pending, POST_INSTALL_DONE) {
            self.post_install_done = true;
            return Ok(());
        }
        let keep = POST_INSTALL_DONE.len().max(COMMAND_FAILED.len()) - 1;
        if self.post_install_pending.len() > keep {
            self.post_install_pending
                .drain(..self.post_install_pending.len() - keep);
        }
        Ok(())
    }

    fn distribution_choice(&mut self, bytes: &[u8]) -> Option<u8> {
        if self.menu_answered || self.phase != InstallPhase::ReadingSources {
            return None;
        }
        self.menu_pending.extend_from_slice(bytes);
        if self.menu_pending.len() > 4096 {
            self.menu_pending.drain(..self.menu_pending.len() - 4096);
        }
        if !self.menu_pending.ends_with(b"] ") {
            return None;
        }
        let output = String::from_utf8_lossy(&self.menu_pending);
        let menu = output.rfind("Install software from: [")?;
        let item = output[..menu]
            .lines()
            .rev()
            .find(|line| line.contains(" done (distribution information read"))?;
        let choice = item.split_whitespace().next()?.parse().ok()?;
        self.menu_answered = true;
        Some(choice)
    }

    fn observe_conflicts(&mut self, bytes: &[u8]) -> bool {
        const MARKER: &[u8] = b"ERROR: Conflicts must be resolved.";
        if self.conflict_seen {
            self.conflict_prompt_pending.extend_from_slice(bytes);
        } else {
            self.conflict_pending.extend_from_slice(bytes);
            if let Some(position) = self
                .conflict_pending
                .windows(MARKER.len())
                .position(|window| window == MARKER)
            {
                self.conflict_seen = true;
                self.conflict_prompt_pending
                    .extend_from_slice(&self.conflict_pending[position + MARKER.len()..]);
                self.conflict_pending.clear();
            } else {
                let keep = MARKER.len().saturating_sub(1);
                if self.conflict_pending.len() > keep {
                    self.conflict_pending
                        .drain(..self.conflict_pending.len() - keep);
                }
                return false;
            }
        }
        if self.conflict_prompt_pending.ends_with(INSTALL_PROMPT) {
            return true;
        }
        let keep = INSTALL_PROMPT.len().saturating_sub(1);
        if self.conflict_prompt_pending.len() > keep {
            self.conflict_prompt_pending
                .drain(..self.conflict_prompt_pending.len() - keep);
        }
        false
    }
    fn observe(&mut self, bytes: &[u8]) -> bool {
        self.pending.extend_from_slice(bytes);
        loop {
            let marker: &[u8] = match self.phase {
                InstallPhase::ReadingSources => b"Installing/removing files ..",
                InstallPhase::Transferring => b"Checking dependencies ..",
                InstallPhase::Checking => b"Calculating sizes ..",
                InstallPhase::Finalizing if self.pass < self.passes => {
                    b"Installing/removing files .."
                }
                InstallPhase::Finalizing => {
                    if self.pending.ends_with(INSTALL_PROMPT) {
                        return true;
                    }
                    let keep = INSTALL_PROMPT.len() - 1;
                    if self.pending.len() > keep {
                        self.pending.drain(..self.pending.len() - keep);
                    }
                    return false;
                }
            };
            let Some(position) = self.pending.windows(marker.len()).position(|w| w == marker)
            else {
                let keep = marker.len().saturating_sub(1);
                if self.pending.len() > keep {
                    self.pending.drain(..self.pending.len() - keep);
                }
                return false;
            };
            self.pending.drain(..position + marker.len());
            self.phase = match self.phase {
                InstallPhase::ReadingSources => InstallPhase::Transferring,
                InstallPhase::Transferring => InstallPhase::Checking,
                InstallPhase::Checking => InstallPhase::Finalizing,
                InstallPhase::Finalizing => {
                    self.pass += 1;
                    InstallPhase::Transferring
                }
            };
        }
    }
}

impl InstallConsole {
    /// Follow `passes` runs of go back to Inst>. Only the first pass reopens
    /// the primary distribution, so only it can show the distribution menu.
    fn wait_for_install(&mut self, passes: usize, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut progress = InstallProgress::new(passes);
        let mut chunk = [0u8; 4096];
        while Instant::now() < deadline {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("guest serial console closed during installation".into()),
                Ok(count) => {
                    io::stdout().write_all(&chunk[..count])?;
                    io::stdout().flush()?;
                    let conflict_prompt = progress.observe_conflicts(&chunk[..count]);
                    if let Some(choice) = progress.distribution_choice(&chunk[..count]) {
                        self.stream.write_all(format!("{choice}\n").as_bytes())?;
                    }
                    if conflict_prompt {
                        return Err("IRIX reported unresolved installation conflicts; guest remains running".into());
                    }
                    progress.observe_post_install(&chunk[..count])?;
                    if progress.observe(&chunk[..count]) {
                        if !progress.post_install_done {
                            return Err("inst returned without finishing the post-install commands; guest remains running".into());
                        }
                        return Ok(());
                    }
                }
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(format!(
            "guest installation did not finish after {} seconds (pass {} of {}, {:?}); machine remains running",
            timeout.as_secs(),
            progress.pass,
            progress.passes,
            progress.phase
        )
        .into())
    }
}

/// The command that loads the install script, and how many passes of `go`
/// it makes.
fn install_script(media: &InstallMedia) -> (String, usize) {
    // irix.cmds runs a second go for the add-ons when any are configured.
    let passes = if media.addons.is_empty() { 1 } else { 2 };
    (
        format!("admin source 10.98.0.2:/{STANDARD_INSTALL_SCRIPT}.cmds\n"),
        passes,
    )
}

/// The quit output stops at the restart prompt. IRIX relinks the kernel
/// there, and an lboot message means that kernel may be missing objects.
fn check_quit(output: &str) -> Result<()> {
    if let Some(line) = output.lines().find(|line| line.contains("lboot")) {
        return Err(format!(
            "kernel reconfiguration reported: {}; guest remains at the restart prompt",
            line.trim()
        )
        .into());
    }
    Ok(())
}

pub fn apply(dir: &Path, file: &MachineFile) -> Result<()> {
    if file.network.mode != "private" {
        return Err("package installation requires private installation networking".into());
    }
    let media = read_media(dir)?;
    let (command, passes) = install_script(&media);
    let mut console = InstallConsole::connect(dir)?;
    println!("Load the package script only after the guest reaches Inst>. Disconnect any interactive console first.");
    console.command("", INSTALL_PROMPT, Duration::from_secs(30))?;
    console.stream.write_all(command.as_bytes())?;
    console.wait_for_install(passes, Duration::from_secs(4 * 60 * 60))?;
    let quit = console.command("quit", RESTART_PROMPT, Duration::from_secs(900))?;
    check_quit(&quit)?;
    console.restart()?;
    println!("Installed IRIX reached serial login. Stop the machine before switching to user networking.");
    Ok(())
}

impl InstallConsole {
    /// Answer the restart prompt and wait for login. If the first boot's
    /// autoconfig built a kernel instead of the quit-time reconfiguration,
    /// log in as root, which has no password on a new install, and restart
    /// once more so the guest runs that kernel.
    fn restart(&mut self) -> Result<()> {
        let boot = self.command("y", SERIAL_LOGIN_PROMPT_SUFFIX, Duration::from_secs(900))?;
        if !boot.contains(STARTUP_REBUILD) {
            return Ok(());
        }
        println!("\nFirst boot rebuilt the kernel. Restarting once more to load it.");
        self.command("root", TERMINAL_PROMPT, Duration::from_secs(60))?;
        self.command("", SHELL_PROMPT, Duration::from_secs(60))?;
        self.command(
            "reboot",
            SERIAL_LOGIN_PROMPT_SUFFIX,
            Duration::from_secs(900),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first-pass selection of the shipped `irix` script.
    fn selection(recipe: &InstallRecipe, field: &str) -> Vec<String> {
        recipe.document["install_scripts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|script| script["name"] == "irix")
            .unwrap()[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_owned())
            .collect()
    }

    fn set_names(recipe: &InstallRecipe) -> Vec<&str> {
        recipe.document["install_sets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|set| set["name"].as_str().unwrap())
            .collect()
    }

    /// The configuration as Instigator loads it: each add-on file becomes the
    /// install set `addon-NAME`, and every script gains a second pass over the
    /// add-on sets, in file order.
    fn with_addons_loaded(config: &InstallConfig) -> Value {
        let mut document = config.document.clone();
        let mut sets = Vec::new();
        let mut install = Vec::new();
        for (_, text) in &config.addon_files {
            let addon: Value = serde_saphyr::from_str(text).unwrap();
            let name = format!("addon-{}", addon["name"].as_str().unwrap());
            document["install_sets"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "name": name,
                    "addon": true,
                    "layers": addon["layers"],
                }));
            sets.push(json!(name));
            install.extend(addon["install"].as_array().unwrap().iter().cloned());
        }
        document.as_object_mut().unwrap().remove("addons");
        if !sets.is_empty() {
            for script in document["install_scripts"].as_array_mut().unwrap() {
                script["then"] = json!({ "sets": sets, "install": install });
            }
        }
        document
    }

    #[test]
    fn embedded_recipe_has_https_layer_urls() {
        let recipe = recipe(DEFAULT_RELEASE, "standard").unwrap();
        for layer in &recipe.layers {
            crate::assets::validate_https_url(&layer.url).unwrap();
        }
        crate::assets::validate_https_url(&named_addon("mipspro", None).unwrap().source).unwrap();
    }

    #[test]
    fn install_apply_returns_on_explicit_conflict_from_inst_output() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let writer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for chunk in [
                b"No matches for \"java_dev.sw.base\" were found\r\nInst> ".as_slice(),
                b"ERROR: Conflicts must be",
                b" resolved.\r\n\ninventor_dev.sw.base cannot be installed because of missing prerequisites\r\njava2_plugin.sw32.mozilla_freeware cannot be installed because of missing prerequisites\r\nInst> ",
            ] {
                stream.write_all(chunk).unwrap();
                stream.flush().unwrap();
                std::thread::sleep(Duration::from_millis(25));
            }
            std::thread::sleep(Duration::from_millis(150));
        });
        let mut console = InstallConsole {
            stream: TcpStream::connect(address).unwrap(),
        };
        let error = console
            .wait_for_install(1, Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        writer.join().unwrap();
        assert!(error.contains("guest remains running"), "{error}");
    }

    #[test]
    fn install_conflict_is_reported_at_prompt_after_diagnostics() {
        let mut progress = InstallProgress::new(1);
        assert!(!progress.observe_conflicts(b"Inst> "));

        assert!(!progress.observe_conflicts(b"ERROR: Conflicts must be"));
        assert!(!progress
            .observe_conflicts(b" resolved.\r\ninventor_dev.sw.base missing prerequisites\r\n"));
        assert!(progress.conflict_seen);
        assert!(!progress
            .observe_conflicts(b"java2_plugin.sw32.mozilla_freeware missing prerequisites\r\nIns"));
        assert!(progress.observe_conflicts(b"t> "));
    }

    #[test]
    fn install_conflict_marker_is_detected_across_every_split() {
        let marker = b"ERROR: Conflicts must be resolved.";
        for split in 0..=marker.len() {
            let mut progress = InstallProgress::new(1);
            let first = progress.observe_conflicts(&marker[..split]);
            assert!(!first, "split {split}");
            let second = progress.observe_conflicts(&marker[split..]);
            assert!(!second, "split {split}");
            assert!(progress.conflict_seen, "split {split}");
        }
    }

    #[test]
    fn install_progress_selects_distribution_menu_once() {
        let mut progress = InstallProgress::new(1);
        assert_eq!(progress.distribution_choice(b" 1 primary\r\n 7 done (distribution information read, return to inst prompt)\r\nInstall software from: [primary"), None);
        assert_eq!(progress.distribution_choice(b"] "), Some(7));
        assert_eq!(
            progress.distribution_choice(b"Install software from: [primary] "),
            None
        );
    }

    #[test]
    fn install_progress_ignores_intermediate_inst_prompt() {
        let mut progress = InstallProgress::new(1);
        assert!(!progress.observe(b"Inst> Reading product descriptions .. 100% Done.\r\n"));
        assert!(!progress.observe(b"Inst> "));
        assert_eq!(progress.phase, InstallPhase::ReadingSources);
        assert!(!progress.observe(b"Installing/removing fi"));
        assert!(!progress.observe(b"les .. 22%\r\n"));
        assert_eq!(progress.phase, InstallPhase::Transferring);
        assert!(!progress.observe(b"Checking dependencies .. 97% 100% Done.\r\n"));
        assert_eq!(progress.phase, InstallPhase::Checking);
        assert!(!progress.observe(b"Calculating sizes .. 100% Done.\r\nInst>"));
        assert!(progress.observe(b" "));
    }

    /// Serve `chunks` to an InstallConsole, pausing between them so each
    /// arrives as its own read, then stay connected past the one-second
    /// timeouts the tests use.
    fn scripted_console(
        chunks: Vec<&'static [u8]>,
    ) -> (InstallConsole, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let writer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for chunk in chunks {
                stream.write_all(chunk).unwrap();
                stream.flush().unwrap();
                std::thread::sleep(Duration::from_millis(25));
            }
            std::thread::sleep(Duration::from_millis(1500));
        });
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        (InstallConsole { stream }, writer)
    }

    const PASS: &[u8] = b"Installing/removing files .. 22%\r\nChecking dependencies .. 100% Done.\r\nCalculating sizes .. 100% Done.\r\n";
    // As captured from IRIX 6.5.30 inst after the last go.
    const POST_INSTALL_OUTPUT: &[u8] = b"\x07\x07\x07\r\nchrooting to /root\r\n\r\nCalculating sizes ..   0% 100% Done.\r\n\r\ninstigator: post-install commands finished\r\n\r\n\r\n";

    #[test]
    fn install_apply_waits_for_the_second_pass_to_return() {
        // A prompt after the first pass does not end a two-pass install.
        let (mut console, writer) = scripted_console(vec![PASS, b"\r\nInst> "]);
        let error = console
            .wait_for_install(2, Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        writer.join().unwrap();
        assert!(error.contains("pass 1 of 2"), "{error}");

        // As captured from IRIX 6.5.30 inst: the second pass starts reading
        // with no prompt in between, and only the last pass returns to Inst>.
        let (mut console, writer) = scripted_console(vec![
            PASS,
            b"\x07\x07\x07Reading product descriptions ..   0% 100% Done.\r\n",
            PASS,
            POST_INSTALL_OUTPUT,
            b"Inst> ",
        ]);
        console.wait_for_install(2, Duration::from_secs(5)).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn install_apply_requires_the_post_install_commands_to_finish() {
        let (mut console, writer) = scripted_console(vec![
            PASS,
            b"\r\nchrooting to /root\r\nchkconfig: not found\r\n(command fa",
            b"iled)\r\nInst> ",
        ]);
        let error = console
            .wait_for_install(1, Duration::from_secs(5))
            .unwrap_err()
            .to_string();
        writer.join().unwrap();
        assert!(error.contains("post-install"), "{error}");

        let (mut console, writer) = scripted_console(vec![PASS, b"\r\nInst> "]);
        let error = console
            .wait_for_install(1, Duration::from_secs(5))
            .unwrap_err()
            .to_string();
        writer.join().unwrap();
        assert!(error.contains("post-install"), "{error}");

        let marker = POST_INSTALL_DONE;
        for split in 0..=marker.len() {
            let mut progress = InstallProgress::new(1);
            progress.observe_post_install(&marker[..split]).unwrap();
            progress.observe_post_install(&marker[split..]).unwrap();
            assert!(progress.post_install_done, "split {split}");
        }
    }

    #[test]
    fn install_apply_reports_a_conflict_in_the_second_pass() {
        let (mut console, writer) = scripted_console(vec![
            PASS,
            b"Reading product descriptions .. 100% Done.\r\n",
            b"ERROR: Conflicts must be resolved.\r\nrad4x.sw.driver cannot be installed\r\nInst> ",
        ]);
        let error = console
            .wait_for_install(2, Duration::from_secs(5))
            .unwrap_err()
            .to_string();
        writer.join().unwrap();
        assert!(error.contains("conflicts"), "{error}");
    }

    #[test]
    fn install_apply_stops_when_quit_never_reaches_restart() {
        let (mut console, writer) = scripted_console(vec![b"Requickstarting ELF files .. 15% "]);
        let error = console
            .wait_for(RESTART_PROMPT, Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        writer.join().unwrap();
        assert!(error.contains("machine remains running"), "{error}");
    }

    #[test]
    fn install_apply_stops_on_an_lboot_message_at_quit() {
        check_quit("Requickstarting ELF files .. 100% Done.\r\nAutomatically reconfiguring the operating system.\r\nReady to restart the system.  Restart? ").unwrap();
        let error = check_quit("Automatically reconfiguring the operating system.\r\nlboot: Unresolved symbol rad4pci_init\r\nReady to restart the system.")
            .unwrap_err()
            .to_string();
        assert!(error.contains("rad4pci_init"), "{error}");
    }

    #[test]
    fn restart_logs_in_and_reboots_after_a_first_boot_rebuild() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let guest = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = io::BufReader::new(stream);
            let mut received = Vec::new();
            for reply in [
                "The system is coming up.\r\nAutomatically reconfiguring the operating system.\r\nReboot to start using the reconfigured kernel.\r\nIRIS console login: ",
                "IRIX Release 6.5 IP27 IRIS\r\nTERM = (vt100) ",
                "IRIS 1# ",
                "The system is shutting down.\r\nIRIS console login: ",
            ] {
                let mut line = String::new();
                io::BufRead::read_line(&mut reader, &mut line).unwrap();
                received.push(line.trim_end().to_owned());
                writer.write_all(reply.as_bytes()).unwrap();
            }
            received
        });
        let mut console = InstallConsole {
            stream: TcpStream::connect(address).unwrap(),
        };
        console.restart().unwrap();
        assert_eq!(guest.join().unwrap(), ["y", "root", "", "reboot"]);
    }

    #[test]
    fn restart_needs_no_login_when_quit_built_the_kernel() {
        let (mut console, writer) = scripted_console(vec![
            b"RAD4 driver 1.101\r\nThe system is coming up.\r\nIRIS console login: ",
        ]);
        console.restart().unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn restart_accepts_a_configured_hostname_login_prompt() {
        let hostname = "origami-public-test";
        let expected = format!("{hostname} console login: ");
        let writer_expected = expected.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let writer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(writer_expected.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        let mut console = InstallConsole {
            stream: TcpStream::connect(address).unwrap(),
        };
        let output = console
            .wait_for(SERIAL_LOGIN_PROMPT_SUFFIX, Duration::from_secs(5))
            .unwrap();
        writer.join().unwrap();
        assert_eq!(output, expected);
    }

    #[test]
    fn addon_scripts_install_after_the_profile() {
        use crate::{Machine, Network};
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
            profile: "standard".into(),
            release: DEFAULT_RELEASE.into(),
            media: recipe(DEFAULT_RELEASE, "standard")
                .unwrap()
                .layers
                .iter()
                .map(|layer| (layer.name.clone(), source.display().to_string()))
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
            format: crate::MACHINE_FORMAT,
            machine: Machine {
                model: "origin200".into(),
                nodes: 1,
                cpus_per_node: 1,
                memory_per_node: "256MiB".into(),
                console: None,
                processor: None,
                topology: None,
                population: vec![],
                inputs: Default::default(),
            },
            identity: Some(crate::Identity {
                mac: "08:00:69:12:34:56".into(),
            }),
            network: Network {
                mode: "private".into(),
                endpoint: Some("install/network.sock".into()),
                interface: None,
                forward: vec![],
                share_address: None,
            },
            pipe: vec![crate::graphics::Pipe {
                slot: "pci-5".into(),
                family: "psitech".into(),
                product: "rad4".into(),
                settings: Default::default(),
            }],
            drive: vec![],
            share: vec![],
            qemu: Default::default(),
        };
        let config_one = config(&dir, &file, &media).unwrap();
        let document = with_addons_loaded(&config_one);
        let scripts = document["install_scripts"].as_array().unwrap();
        // One script, whatever the add-ons: Instigator gives it the second pass.
        assert_eq!(scripts.len(), 1);
        let irix = &scripts[0];
        assert_eq!(irix["name"], "irix");
        assert_eq!(
            irix["keep"],
            json!(selection(
                &recipe(DEFAULT_RELEASE, "standard").unwrap(),
                "keep"
            ))
        );
        let addon_set = &document["install_sets"]
            [set_names(&recipe(DEFAULT_RELEASE, "standard").unwrap()).len()];
        assert_eq!(addon_set["name"], "addon-tablet");
        assert_eq!(
            addon_set["layers"][0]["source"],
            source.display().to_string()
        );
        assert_eq!(addon_set["layers"][0]["base"], "tablet-disc");
        assert_eq!(addon_set["layers"][0]["dist"], "dist");
        assert_eq!(addon_set["addon"], true);
        // The first pass is the profile alone. The add-on waits for the
        // second pass, so its files land after IRIX's own.
        assert_eq!(irix["install"], json!(["nfs.sw.nfs"]));
        assert_eq!(irix["then"]["sets"], json!(["addon-tablet"]));
        assert_eq!(irix["then"]["install"], json!(["tablet.sw.helper"]));

        // Instigator reads the add-on from the file the configuration names.
        let (path, _) = &config_one.addon_files[0];
        assert!(path.is_absolute());
        assert_eq!(
            config_one.document["addons"],
            json!([path.display().to_string()])
        );
        config_one.write_addon_files().unwrap();
        assert!(path.ends_with("install/addons/tablet.yaml"));
        let written: Value = serde_saphyr::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(written["name"], "tablet");
        assert_eq!(written["install"], json!(["tablet.sw.helper"]));

        // install-apply installs every add-on together.
        let mut two = media.clone();
        two.addons.push(named_addon("mipspro", Some(&dir)).unwrap());
        two.addons[1].source = source.display().to_string();
        let document = with_addons_loaded(&config(&dir, &file, &two).unwrap());
        let irix = &document["install_scripts"][0];
        assert_eq!(
            irix["then"]["sets"],
            json!(["addon-tablet", "addon-mipspro"])
        );
        assert_eq!(irix["then"]["install"][0], "tablet.sw.helper");
        assert_eq!(irix["then"]["install"][1], "c++_dev");
        assert_eq!(
            install_script(&two),
            ("admin source 10.98.0.2:/irix.cmds\n".to_string(), 2)
        );
        assert_eq!(irix["post_install"], json!(POST_INSTALL));
        let mut invalid = media.clone();
        invalid.addons[0].install = vec!["tablet.sw.helper\nquit".into()];
        assert!(config(&dir, &file, &invalid).is_err());
        invalid.addons[0].install = vec!["tablet.sw.helper".into()];
        invalid.addons.push(invalid.addons[0].clone());
        assert!(config(&dir, &file, &invalid).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn add_addon_preserves_https_source_and_rejects_invalid_urls_without_writing() {
        use crate::{Machine, Network};
        use std::time::{SystemTime, UNIX_EPOCH};

        let dir = std::env::temp_dir().join(format!(
            "sgi-remote-addon-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("install")).unwrap();
        let source = dir.join("tablet.tardist");
        fs::write(&source, b"synthetic source").unwrap();
        let media = InstallMedia {
            format: 1,
            profile: "standard".into(),
            release: DEFAULT_RELEASE.into(),
            media: recipe(DEFAULT_RELEASE, "standard")
                .unwrap()
                .layers
                .iter()
                .map(|layer| (layer.name.clone(), source.display().to_string()))
                .collect(),
            addons: vec![],
        };
        let manifest_path = dir.join("install/media.toml");
        fs::write(&manifest_path, toml::to_string_pretty(&media).unwrap()).unwrap();

        let url = "https://origami-dist.irix.fans/irix/addons/tablet/tablet.tardist";
        add_addon(
            &dir,
            "tablet",
            url,
            Some("tablet-disc"),
            Some("dist"),
            &["tablet.sw.helper".into()],
        )
        .unwrap();
        let configured = read_media(&dir).unwrap();
        assert_eq!(configured.addons[0].source, url);

        let file = MachineFile {
            format: crate::MACHINE_FORMAT,
            machine: Machine {
                model: "origin200".into(),
                nodes: 1,
                cpus_per_node: 1,
                memory_per_node: "256MiB".into(),
                console: None,
                processor: None,
                topology: None,
                population: vec![],
                inputs: Default::default(),
            },
            identity: Some(crate::Identity {
                mac: "08:00:69:12:34:56".into(),
            }),
            network: Network {
                mode: "private".into(),
                endpoint: Some("install/network.sock".into()),
                interface: None,
                forward: vec![],
                share_address: None,
            },
            pipe: vec![crate::graphics::Pipe {
                slot: "pci-5".into(),
                family: "psitech".into(),
                product: "rad4".into(),
                settings: Default::default(),
            }],
            drive: vec![],
            share: vec![],
            qemu: Default::default(),
        };
        let document = with_addons_loaded(&config(&dir, &file, &configured).unwrap());
        let addon_index = set_names(&recipe(DEFAULT_RELEASE, "standard").unwrap()).len();
        let addon_set = &document["install_sets"][addon_index];
        assert_eq!(addon_set["layers"][0]["source"], url);
        assert_eq!(addon_set["layers"][0]["base"], "tablet-disc");
        assert_eq!(
            document["install_scripts"][0]["then"]["install"],
            json!(["tablet.sw.helper"])
        );

        add_addon(
            &dir,
            "local-tablet",
            source.to_str().unwrap(),
            None,
            None,
            &["tablet.sw.helper".into()],
        )
        .unwrap();
        assert_eq!(
            read_media(&dir).unwrap().addons[1].source,
            source.canonicalize().unwrap().display().to_string()
        );

        let before = fs::read(&manifest_path).unwrap();
        for invalid in [
            "https://[",
            "http://origami-dist.irix.fans/tablet.tardist",
            "https://user@origami-dist.irix.fans/tablet.tardist",
            "https://origami-dist.irix.fans/tablet.tardist?token=secret",
        ] {
            assert!(add_addon(
                &dir,
                "invalid",
                invalid,
                None,
                None,
                &["tablet.sw.helper".into()],
            )
            .is_err());
            assert_eq!(fs::read(&manifest_path).unwrap(), before);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn media_root_names_the_directory_containing_release_folders() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "sgi-media-root-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let media_root = root.join("media");
        fs::create_dir_all(media_root.join("mipspro/7.4.4")).unwrap();
        fs::write(
            media_root.join("mipspro/7.4.4/mipspro744update.tar.gz"),
            b"synthetic archive",
        )
        .unwrap();
        let prom = root.join("prom.bin");
        fs::write(&prom, vec![0; 1048576]).unwrap();
        let machine = root.join("machine");
        let machines = crate::test_machines();
        let offer = crate::preset(&machines, "origin200-1").unwrap();
        let tool = crate::test_support::init_tool(&root);
        let init = crate::MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: None,
        };
        crate::create_configured(
            &machine,
            offer,
            &init,
            None,
            None,
            vec![],
            Default::default(),
        )
        .unwrap();
        let mut file = crate::read_machine(&machine).unwrap();
        init_profile(
            &machines,
            &machine,
            &media_root,
            Some("08:00:69:12:34:56"),
            &mut file,
            DEFAULT_RELEASE,
            "standard",
        )
        .unwrap();
        add_known_addon(&machine, "mipspro", Some(&media_root)).unwrap();
        let manifest = read_media(&machine).unwrap();
        assert!(file
            .network
            .endpoint
            .as_deref()
            .unwrap()
            .starts_with("tcp:127.0.0.1:"));
        let expected_root = media_root.canonicalize().unwrap();
        assert_eq!(
            manifest.media["overlays1"],
            expected_root
                .join("6.5.30/overlays1.image")
                .display()
                .to_string()
        );
        assert_eq!(
            manifest.addons[0].source,
            expected_root
                .join("mipspro/7.4.4/mipspro744update.tar.gz")
                .display()
                .to_string()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn remote_init_preserves_recipe_without_downloading() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let root = std::env::temp_dir().join(format!(
            "origami-remote-install-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let prom = root.join("synthetic-prom.bin");
        fs::write(&prom, [0u8; 1]).unwrap();
        let machines = crate::test_machines();
        let offer = crate::preset(&machines, "origin200-1").unwrap();
        let dir = root.join("machine");
        let tool = crate::test_support::init_tool(&root);
        let init = crate::MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: None,
        };
        crate::create_configured(&dir, offer, &init, None, None, vec![], Default::default())
            .unwrap();
        let mut file = crate::read_machine(&dir).unwrap();
        init_remote_profile(
            &machines,
            &dir,
            Some("08:00:69:12:34:56"),
            &mut file,
            DEFAULT_RELEASE,
            "standard",
        )
        .unwrap();
        let mut media = read_media(&dir).unwrap();
        assert_eq!(media.media.len(), 10);
        assert_eq!(
            media.media["overlays1"],
            "https://origami-dist.irix.fans/irix/6.5.30/overlays1.iso"
        );
        let document = config(&dir, &file, &media).unwrap().document;
        // The installed system is named after the machine directory.
        assert_eq!(document["clients"][0]["hostname"], "machine");
        // Instigator takes at most 40 characters, and IRIX needs a valid name.
        for name in ["a".repeat(41), "has_underscore".into(), "-leading".into()] {
            let document = config(&root.join(name), &file, &media).unwrap().document;
            assert!(document["clients"][0].get("hostname").is_none());
        }
        let document = config(&dir, &file, &media).unwrap().document;
        let sets = document["install_sets"].as_array().unwrap();
        assert_eq!(sets.len(), 5);
        assert_eq!(document["install_sets"][0]["layers"][0]["boot"], true);
        let development = sets
            .iter()
            .find(|set| set["name"] == "development")
            .unwrap();
        let runtime_layers: Vec<_> = development["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| layer["name"].as_str().unwrap())
            .collect();
        assert_eq!(runtime_layers, ["devlibs", "devfoundation"]);
        assert!(sets
            .iter()
            .flat_map(|set| set["layers"].as_array().unwrap())
            .all(|layer| !layer["source"].as_str().unwrap().ends_with(".tar.gz")));
        assert!(media.media.values().all(|source| source.ends_with(".iso")));
        assert_eq!(
            document["install_scripts"][0]["install"],
            json!(["nfs.sw.nfs"])
        );
        assert_eq!(
            install_script(&media),
            (
                format!(
                    "admin source 10.98.0.2:/{}.cmds\n",
                    document["install_scripts"][0]["name"].as_str().unwrap()
                ),
                1
            )
        );
        assert!(media.media.values().all(|source| source.ends_with(".iso")));
        for invalid in [
            "http://example.invalid/media.image",
            "https://user:password@example.invalid/media.image",
            "https://example.invalid/media.image?token=secret",
            "ftp://example.invalid/media.image",
        ] {
            media.media.insert("overlays1".into(), invalid.into());
            assert!(config(&dir, &file, &media).is_err(), "{invalid}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rad4_machine_init_configures_the_rad4_addon() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let prom = root.join("synthetic-prom.bin");
        fs::write(&prom, [0u8; 1]).unwrap();
        let machines = crate::test_machines();
        let offer = crate::preset(&machines, "origin200-1").unwrap();
        let tool = crate::test_support::init_tool(root);
        let init = crate::MachineInit {
            tool: &tool,
            boot_prom: &prom,
            io_prom: None,
        };
        for (index, graphics) in ["none", "rad4"].into_iter().enumerate() {
            let dir = root.join(graphics);
            // Machines side by side need different addresses.
            let mac = format!("08:00:69:12:34:5{index}");
            let pipes = crate::graphics::select(
                &machines,
                &offer.topology,
                offer.nodes,
                &crate::graphics::Request {
                    product: Some(graphics),
                    ..Default::default()
                },
            )
            .unwrap();
            crate::create_configured(&dir, offer, &init, None, None, pipes, Default::default())
                .unwrap();
            let mut file = crate::read_machine(&dir).unwrap();
            init_remote_profile(
                &machines,
                &dir,
                Some(&mac),
                &mut file,
                DEFAULT_RELEASE,
                "standard",
            )
            .unwrap();
            let media = read_media(&dir).unwrap();
            let document = with_addons_loaded(&config(&dir, &file, &media).unwrap());
            let irix = &document["install_scripts"][0];
            assert_eq!(irix["name"], "irix");
            if graphics == "none" {
                assert!(media.addons.is_empty());
                assert!(irix.get("then").is_none());
                continue;
            }
            let addon = &media.addons[0];
            assert_eq!(addon.name, "rad4");
            assert_eq!(
                addon.source,
                "https://origami-dist.irix.fans/irix/addons/rad4/rad4x_65_13k.tar.gz"
            );
            assert_eq!(addon.dist.as_deref(), Some("dist_6.5"));
            assert_eq!(
                addon.install,
                ["rad4x.sw.driver", "rad4x.sw.xserver", "rad4x.man"]
            );
            // irix.cmds is the script the user guide names, so on a RAD4
            // machine it must install the driver too.
            assert_eq!(irix["then"]["sets"], json!(["addon-rad4"]));
            assert_eq!(irix["then"]["install"], json!(addon.install));
            assert_eq!(
                install_script(&media),
                ("admin source 10.98.0.2:/irix.cmds\n".to_string(), 2)
            );
        }
    }

    #[test]
    fn minimal_and_standard_profiles_use_discs_and_standard_selections() {
        for (name, count) in [("minimal", 6), ("standard", 10)] {
            let recipe = recipe(DEFAULT_RELEASE, name).unwrap();
            assert_eq!(recipe.layers.len(), count);
            assert!(recipe
                .layers
                .iter()
                .all(|layer| layer.url.ends_with(".iso")));
            assert_eq!(selection(&recipe, "install"), ["nfs.sw.nfs"]);
            assert!(recipe.document["install_sets"][0]["layers"][0]["boot"]
                .as_bool()
                .unwrap());
        }
        assert!(recipe(DEFAULT_RELEASE, "missing").is_err());
        assert_eq!(
            selection(&recipe(DEFAULT_RELEASE, "base").unwrap(), "keep"),
            selection(&recipe(DEFAULT_RELEASE, "minimal").unwrap(), "keep")
        );
        assert_eq!(
            selection(&recipe(DEFAULT_RELEASE, "desktop").unwrap(), "keep"),
            selection(&recipe(DEFAULT_RELEASE, "standard").unwrap(), "keep")
        );
        for removed in ["development", "legacy-development"] {
            let error = recipe(DEFAULT_RELEASE, removed).err().unwrap().to_string();
            assert!(error.contains("--name mipspro"), "{error}");
        }
    }

    #[test]
    fn profiles_deselect_conflicting_products() {
        assert_eq!(
            selection(&recipe(DEFAULT_RELEASE, "standard").unwrap(), "keep"),
            [
                "inventor_dev",
                "java2_plugin.sw32.mozilla_freeware",
                "java_dev.sw32.binaries"
            ]
        );
        assert_eq!(
            selection(&recipe(DEFAULT_RELEASE, "minimal").unwrap(), "keep"),
            [
                "desktop_base.sw.dso6_2",
                "insight.sw.sgihelp",
                "inventor_dev"
            ]
        );
    }

    #[test]
    fn local_media_paths_mirror_the_public_layout() {
        let recipe = recipe(DEFAULT_RELEASE, "standard").unwrap();
        let paths: Vec<_> = recipe
            .layers
            .iter()
            .map(|layer| layer.local_path().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "6.5.30/overlays1.image",
                "6.5.30/overlays2.image",
                "6.5.30/overlays3.image",
                "6.5-base/foundation1.image",
                "6.5-base/foundation2.image",
                "6.5-base/nfs.image",
                "6.5-base/devlibs.image",
                "6.5-base/devfoundation.image",
                "6.5.30/applications.image",
                "6.5.30/complementary.image",
            ]
        );
        let root = Path::new("/media");
        assert_eq!(
            named_addon("mipspro", Some(root)).unwrap().source,
            "/media/mipspro/7.4.4/mipspro744update.tar.gz"
        );
        let error = named_addon("rad4", Some(root)).unwrap_err().to_string();
        assert!(error.contains("no local media"), "{error}");
        assert!(named_addon("nothing", None).is_err());
    }

    fn golden_media(release: &str, recipe: &InstallRecipe) -> InstallMedia {
        InstallMedia {
            format: 1,
            profile: "standard".into(),
            release: release.into(),
            media: recipe
                .layers
                .iter()
                .map(|layer| (layer.name.clone(), layer.url.clone()))
                .collect(),
            addons: vec![],
        }
    }

    fn golden_config(media: &InstallMedia) -> InstallConfig {
        use crate::{Machine, Network};
        let file = MachineFile {
            format: crate::MACHINE_FORMAT,
            machine: Machine {
                model: "origin200".into(),
                nodes: 1,
                cpus_per_node: 1,
                memory_per_node: "256MiB".into(),
                console: None,
                processor: None,
                topology: None,
                population: vec![],
                inputs: Default::default(),
            },
            identity: Some(crate::Identity {
                mac: "08:00:69:12:34:56".into(),
            }),
            network: Network {
                mode: "private".into(),
                endpoint: Some("install/network.sock".into()),
                interface: None,
                forward: vec![],
                share_address: None,
            },
            pipe: vec![],
            drive: vec![],
            share: vec![],
            qemu: Default::default(),
        };
        config(Path::new("/machine"), &file, media).unwrap()
    }

    /// What Instigator turns into /irix.cmds: the sets it serves, the first
    /// pass's selections, the add-on second pass and the post-install
    /// commands, with the server settings around them. Script and add-on
    /// bookkeeping that does not reach the generated commands is left out.
    fn effective_install(config: &InstallConfig) -> Value {
        let mut out = with_addons_loaded(config);
        let scripts = out.as_object_mut().unwrap().remove("install_scripts");
        let irix = scripts
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|script| script["name"] == "irix")
            .unwrap()
            .clone();
        out["irix"] = json!({
            "install": irix["install"],
            "keep": irix["keep"],
            "post_install": irix["post_install"],
            "then": irix.get("then").cloned().unwrap_or(Value::Null),
        });
        out
    }

    fn scenario_media(release: &str, profile: &str, addons: &[&str]) -> InstallMedia {
        let mut media = golden_media(release, &recipe(release, profile).unwrap());
        media.profile = profile.into();
        media.addons = addons
            .iter()
            .map(|name| named_addon(name, None).unwrap())
            .collect();
        media
    }

    /// The commands each release, profile and add-on choice generates are
    /// pinned in a fixture written from the earlier recipe-file
    /// implementation, so moving to Instigator's shipped configurations
    /// installs the same products from the same discs.
    #[test]
    fn generated_install_commands_are_unchanged() {
        let mut all = serde_json::Map::new();
        for (release, profile) in [
            ("6.5.30", "minimal"),
            ("6.5.30", "standard"),
            ("6.5.22", "standard"),
        ] {
            for addons in [
                &[][..],
                &["mipspro"][..],
                &["rad4"][..],
                &["mipspro", "rad4"][..],
            ] {
                let media = scenario_media(release, profile, addons);
                all.insert(
                    format!("{release}-{profile}-{}", addons.join("+")),
                    effective_install(&golden_config(&media)),
                );
            }
        }
        let text = serde_json::to_string_pretty(&Value::Object(all)).unwrap() + "\n";
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/install-generated-commands.json");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn release_6_5_22_loads_and_names_its_own_discs() {
        let recipe = recipe("6.5.22", "standard").unwrap();
        assert_eq!(recipe.layers.len(), 10);
        let mut urls = Vec::new();
        for layer in &recipe.layers {
            crate::assets::validate_https_url(&layer.url).unwrap();
            urls.push(layer.url.clone());
        }
        for disc in [
            "overlays1",
            "overlays2",
            "overlays3",
            "applications",
            "complementary",
        ] {
            let url = format!("https://origami-dist.irix.fans/irix/6.5.22/{disc}.iso");
            assert!(urls.contains(&url), "{url}");
        }
        assert!(!urls.iter().any(|url| url.contains("6.5.30")));
        assert_eq!(selection(&recipe, "install"), ["nfs.sw.nfs"]);
        // Products inst could not install on the full media set, from its own
        // conflict list. 6.5.30's keep list names others.
        assert_eq!(
            selection(&recipe, "keep"),
            [
                "java2_plugin.sw32.mozilla_freeware",
                "java_dev.sw32.binaries",
                "sgimeeting.sw.sgitools"
            ]
        );
        let error = super::recipe("6.5.22", "minimal")
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("6.5.22 has no minimal"), "{error}");
        assert!(super::recipe("6.5.7", "standard").is_err());
    }

    #[test]
    fn release_6_5_22_config_resolves_the_overlay_collision_and_adds_addons() {
        let recipe = recipe("6.5.22", "standard").unwrap();
        let mut media = golden_media("6.5.22", &recipe);
        media.addons = vec![
            named_addon("mipspro", None).unwrap(),
            named_addon("rad4", None).unwrap(),
        ];
        assert_eq!(media.addons[0].install.len(), 12);
        assert_eq!(media.addons[1].install[0], "rad4x.sw.driver");
        let document = golden_config(&media).document;
        let sets = document["install_sets"].as_array().unwrap();
        assert_eq!(sets[0]["name"], "6.5.22");
        assert_eq!(sets[0]["layers"][0]["boot"], true);
        assert_eq!(
            sets[0]["collisions"],
            json!({ "6.5.22/dist/.prereq_hints": "overlays1" })
        );
        // Only the set with a collision carries the field.
        assert!(sets[1].get("collisions").is_none());
        assert_eq!(
            document["install_scripts"][0]["keep"],
            json!(selection(&recipe, "keep"))
        );
    }

    #[test]
    fn media_toml_without_a_release_means_6_5_30() {
        let media: InstallMedia = toml::from_str(
            "format = 1\nprofile = \"standard\"\n[media]\noverlays1 = \"https://example.invalid/a.iso\"\n",
        )
        .unwrap();
        assert_eq!(media.release, "6.5.30");
    }
}
