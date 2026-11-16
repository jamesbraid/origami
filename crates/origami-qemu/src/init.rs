//! Creating a machine's stores with QEMU's machine-init tool.

use crate::catalogue::offering_data;
use crate::runtime::Runtime;
use origami_machine::stores::StorePreparer;
use origami_machine::{assets, Offering, Result};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The init tool's command line for a new machine whose storage goes in
/// `state`. The tool refuses an image the machine does not read and a
/// missing one it needs, before it creates anything.
pub fn init_arguments(
    offering: &Offering,
    boot_prom: &Path,
    io_prom: Option<&Path>,
    state: &Path,
) -> Result<Vec<OsString>> {
    let data = offering_data(offering)?;
    let mut args: Vec<OsString> = vec![
        "--machine".into(),
        data.machine_options.clone().into(),
        "--boot-prom".into(),
        boot_prom.into(),
    ];
    if let Some(io_prom) = io_prom {
        args.extend(["--io-prom".into(), io_prom.into()]);
    }
    args.push(state.into());
    Ok(args)
}

/// Run QEMU's init tool, which writes every storage file or nothing.
pub fn create_state(tool: &Path, arguments: &[OsString]) -> Result<()> {
    let output = Command::new(tool)
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot run {}: {error}", tool.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed ({}): {}",
            tool.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

/// The boot and IO PROM images that create `offering`'s storage: the files
/// given, else verified downloads of the kinds its machine list entry reads.
pub fn firmware(
    offering: &Offering,
    boot_prom: Option<&Path>,
    io_prom: Option<&Path>,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let data = offering_data(offering)?;
    let image = |name: &str, given: Option<&Path>, option: &str| -> Result<Option<PathBuf>> {
        if let Some(path) = given {
            return Ok(Some(path.into()));
        }
        let Some(input) = data
            .init_inputs
            .iter()
            .find(|input| input.name == name && input.required)
        else {
            return Ok(None);
        };
        assets::acquire(&input.kind)
            .map(Some)
            .map_err(|error| format!("{error}; supply {option} FILE").into())
    };
    let boot_prom = image("boot-prom", boot_prom, "--prom")?
        .ok_or_else(|| format!("{} reads no boot PROM", offering.topology))?;
    Ok((boot_prom, image("io-prom", io_prom, "--io-prom")?))
}

/// The 256-byte update header that opens an O2 PROM media image, which the
/// flash does not hold. It starts with the bytes `PROM`.
const O2_UPDATE_HEADER: usize = 256;

/// The O2's init tool takes its boot PROM as the flash array's own bytes,
/// but the published image is the media file: an update header, then the
/// code, ending short of the 512 KiB part. The board holds the code from
/// the start of the flash, and erased flash reads 0xFF, which is also the
/// pad byte the machine's `rom-image` loading uses. So the tool gets a copy
/// without the header, padded. Other machines' images are decoded by the
/// tool and pass through.
fn flash_image_boot_prom(
    offering: &Offering,
    boot_prom: &Path,
) -> Result<Option<tempfile::TempPath>> {
    let raw = offering_data(offering)?
        .init_inputs
        .iter()
        .any(|input| input.name == "boot-prom" && input.kind == "ip32-prom");
    let Some(flash) = offering
        .storage
        .iter()
        .find(|item| raw && item.initial == "boot-prom")
    else {
        return Ok(None);
    };
    let mut image = fs::read(boot_prom)
        .map_err(|error| format!("cannot read PROM image {}: {error}", boot_prom.display()))?;
    if image.starts_with(b"PROM") && image.len() > O2_UPDATE_HEADER {
        image.drain(..O2_UPDATE_HEADER);
    }
    if image.len() as u64 > flash.size {
        // The tool names the size it wants.
        return Ok(None);
    }
    image.resize(flash.size as usize, 0xff);
    let mut file = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut file, &image)?;
    // Close the write handle so the init tool can open the file on Windows.
    Ok(Some(file.into_temp_path()))
}

/// QEMU's machine-init tool at `path`.
pub struct InitTool<'a>(pub &'a Path);

impl StorePreparer for InitTool<'_> {
    fn firmware(
        &self,
        offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<(PathBuf, Option<PathBuf>)> {
        firmware(offering, boot_prom, io_prom)
    }

    fn stage_boot_prom(
        &self,
        offering: &Offering,
        boot_prom: &Path,
    ) -> Result<Option<tempfile::TempPath>> {
        flash_image_boot_prom(offering, boot_prom)
    }

    fn create_stores(
        &self,
        offering: &Offering,
        boot_prom: &Path,
        io_prom: Option<&Path>,
        state: &Path,
    ) -> Result<()> {
        create_state(
            self.0,
            &init_arguments(offering, boot_prom, io_prom, state)?,
        )
    }
}

impl StorePreparer for Runtime {
    fn firmware(
        &self,
        offering: &Offering,
        boot_prom: Option<&Path>,
        io_prom: Option<&Path>,
    ) -> Result<(PathBuf, Option<PathBuf>)> {
        firmware(offering, boot_prom, io_prom)
    }

    fn stage_boot_prom(
        &self,
        offering: &Offering,
        boot_prom: &Path,
    ) -> Result<Option<tempfile::TempPath>> {
        flash_image_boot_prom(offering, boot_prom)
    }

    fn create_stores(
        &self,
        offering: &Offering,
        boot_prom: &Path,
        io_prom: Option<&Path>,
        state: &Path,
    ) -> Result<()> {
        InitTool(&self.machine_init()).create_stores(offering, boot_prom, io_prom, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_o2_flash_image_drops_the_media_header_and_pads_with_ff() {
        let machines = crate::test_machines();
        let o2 = origami_machine::preset(&machines, "o2").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media.image");
        let mut bytes = b"PROM".to_vec();
        bytes.resize(O2_UPDATE_HEADER, 0);
        bytes.extend_from_slice(&[1, 2, 3]);
        fs::write(&media, &bytes).unwrap();
        let padded = flash_image_boot_prom(o2, &media).unwrap().unwrap();
        let flash = fs::read(&padded).unwrap();
        assert_eq!(flash.len(), 512 * 1024);
        assert_eq!(flash[..3], [1, 2, 3]);
        assert!(flash[3..].iter().all(|&byte| byte == 0xff));
        // An image without the header is only padded.
        fs::write(&media, [9u8, 8]).unwrap();
        let flash = fs::read(flash_image_boot_prom(o2, &media).unwrap().unwrap()).unwrap();
        assert_eq!(flash[..2], [9, 8]);
        // One that overfills the flash is left for the init tool to refuse.
        fs::write(&media, vec![0u8; 512 * 1024 + 1]).unwrap();
        assert!(flash_image_boot_prom(o2, &media).unwrap().is_none());
        // Other machines' images pass through.
        let fuel = origami_machine::preset(&machines, "fuel-1").unwrap();
        assert!(flash_image_boot_prom(fuel, &media).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_init_tool_is_given_the_machine_options_and_writes_every_store() {
        let machines = crate::test_machines();
        let offer = origami_machine::preset(&machines, "origin200-1").unwrap();
        let root = tempfile::tempdir().unwrap();
        let tool = crate::test_support::init_tool(root.path());
        let prom = root.path().join("prom.img");
        let state = root.path().join("state");
        InitTool(&tool)
            .create_stores(offer, &prom, None, &state)
            .unwrap();
        for item in &offer.storage {
            let file = state.join(format!("{}.raw", item.name));
            assert_eq!(fs::metadata(file).unwrap().len(), item.size);
        }
        let args = fs::read_to_string(format!("{}.args", tool.display())).unwrap();
        assert!(
            args.starts_with("--machine\norigin200,topology=origin200,nodes=1\n"),
            "{args}"
        );
        // A tool that is not there is named in the error.
        let missing = root.path().join("absent");
        let error = InitTool(&missing)
            .create_stores(offer, &prom, None, &root.path().join("other"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot run"), "{error}");
    }
}
