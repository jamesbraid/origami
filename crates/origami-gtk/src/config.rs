//! Command-line options. Everything a test needs to steer is a flag, so the
//! app has no test-only code paths.

use crate::view_scale::ViewScale;
use std::path::PathBuf;

pub struct Config {
    /// Where the library's machine directories live.
    pub library: PathBuf,
    /// Where the app keeps its own settings.
    pub config_dir: PathBuf,
    pub vk_device: Option<String>,
    /// Never import the engine's memory into the GPU: copy changed lines.
    pub no_import: bool,
    /// Serve the automation protocol on this Unix socket.
    pub automation: Option<PathBuf>,
    /// Print what the GPU import can do and exit.
    pub probe: bool,
    /// Start every machine at this view scale instead of its saved one.
    pub view_scale: Option<ViewScale>,
    /// Machines to start as soon as the library is shown, by directory name.
    pub start: Vec<String>,
}

pub const USAGE: &str = "usage: origami-gtk [--library DIR] [--config-dir DIR]
                   [--vk-device NAME] [--no-import] [--view-scale fit|actual|1x|2x]
                   [--automation SOCKET] [--start NAME]... [--probe]";

impl Config {
    pub fn from_args(args: impl Iterator<Item = String>) -> Result<Config, String> {
        let data = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        let mut c = Config {
            library: data.join("origami").join("machines"),
            config_dir: dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("origami"),
            vk_device: None,
            no_import: false,
            automation: None,
            probe: false,
            view_scale: None,
            start: Vec::new(),
        };
        let mut a = args;
        while let Some(k) = a.next() {
            let mut value =
                |what: &str| a.next().ok_or_else(|| format!("{k} needs {what}\n{USAGE}"));
            match k.as_str() {
                "--library" => c.library = value("a directory")?.into(),
                "--config-dir" => c.config_dir = value("a directory")?.into(),
                "--vk-device" => c.vk_device = Some(value("a name")?),
                "--automation" => c.automation = Some(value("a socket path")?.into()),
                "--start" => c.start.push(value("a machine name")?),
                "--no-import" => c.no_import = true,
                "--view-scale" => {
                    let v = value("fit, actual, 1x or 2x")?;
                    c.view_scale = Some(
                        ViewScale::parse(&v)
                            .ok_or_else(|| format!("bad --view-scale {v}\n{USAGE}"))?,
                    );
                }
                "--probe" => c.probe = true,
                "-h" | "--help" => return Err(USAGE.into()),
                other => return Err(format!("unknown argument {other}\n{USAGE}")),
            }
        }
        Ok(c)
    }
}
