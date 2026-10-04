use crate::{qemu_path_option, resolve, MachineFile, Origin300Identity, Result};
use std::fs;
use std::path::Path;

pub fn validate_spd_file(path: &Path) -> Result<()> {
    if !fs::metadata(path)?.is_file() || fs::metadata(path)?.len() != 128 {
        return Err(format!(
            "SPD record {} must be a 128-byte regular file",
            path.display()
        )
        .into());
    }
    fs::File::open(path)?;
    Ok(())
}

pub fn validate_spd(dir: &Path, identity: &Origin300Identity) -> Result<()> {
    if identity.spd_dimm2.is_empty() != identity.spd_dimm3.is_empty() {
        return Err("Origin 300 SPD overrides require both spd_dimm2 and spd_dimm3".into());
    }
    if !identity.spd_dimm2.is_empty() {
        validate_spd_file(&resolve(dir, &identity.spd_dimm2))?;
        validate_spd_file(&resolve(dir, &identity.spd_dimm3))?;
    }
    for value in [&identity.chassis_eeprom, &identity.board_eeprom]
        .into_iter()
        .flatten()
    {
        let path = resolve(dir, value);
        if !fs::metadata(&path).is_ok_and(|info| info.is_file()) {
            return Err(format!("missing identity override {}; restore the supplied record or update its path in machine.toml", path.display()).into());
        }
        fs::File::open(path)?;
    }
    Ok(())
}

pub fn machine_options(dir: &Path, file: &MachineFile) -> Result<String> {
    let identity = file
        .identity
        .as_ref()
        .ok_or("Origin 300 needs an identity section")?;
    let mut options = format!("origin300,io8-mac={}", identity.mac);
    if !identity.spd_dimm2.is_empty() {
        options.push_str(&format!(
            ",spd-dimm2={},spd-dimm3={}",
            qemu_path_option(&resolve(dir, &identity.spd_dimm2)),
            qemu_path_option(&resolve(dir, &identity.spd_dimm3))
        ));
    }
    // Older machines carry explicit records. Preserve their bytes as overrides.
    for (name, property, supplied) in [
        (
            "io8-chassis.bin",
            "chassis-eeprom.1",
            &identity.chassis_eeprom,
        ),
        ("io8-board.bin", "board-eeprom.1", &identity.board_eeprom),
    ] {
        let path = supplied
            .as_ref()
            .map_or_else(|| dir.join("state").join(name), |path| resolve(dir, path));
        match fs::metadata(&path) {
            Ok(info) if info.is_file() => {
                fs::File::open(&path)?;
                options.push_str(&format!(",{property}={}", qemu_path_option(&path)));
            }
            Ok(_) => return Err(format!("identity override {} must be a regular file", path.display()).into()),
            Err(error) if supplied.is_none() && error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(format!("cannot open identity override {}: {error}; restore the supplied record or update its path in machine.toml", path.display()).into()),
        }
    }
    Ok(options)
}
