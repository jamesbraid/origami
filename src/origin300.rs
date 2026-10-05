use crate::{qemu_path_option, resolve, sha256_file, MachineFile, Origin300Identity, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DIMM2_SHA256: &str = "a8bb5857941fefae8e11037fe98b01d199199bd34eb059b269a8c4b6875ca0d3";
pub const DIMM3_SHA256: &str = "9e60c3400772d227b91c5d61cea8de7f89777d41377516de072944c26cef4aa7";
const FLASH_BYTES: usize = 16 * 1024 * 1024;

pub fn validate_spd_file(path: &Path, expected: &str) -> Result<()> {
    if fs::metadata(path)?.len() != 128 || sha256_file(path)? != expected {
        return Err(format!(
            "SPD record {} does not match the reviewed 512 MiB kit",
            path.display()
        )
        .into());
    }
    Ok(())
}

pub fn validate_spd(dir: &Path, identity: &Origin300Identity) -> Result<()> {
    validate_spd_file(&resolve(dir, &identity.spd_dimm2), DIMM2_SHA256)?;
    validate_spd_file(&resolve(dir, &identity.spd_dimm3), DIMM3_SHA256)?;
    Ok(())
}

pub fn flash_path(dir: &Path) -> PathBuf {
    dir.join("state/ip35-boot-flash.raw")
}

pub fn machine_options(dir: &Path, file: &MachineFile) -> Result<String> {
    let identity = file
        .identity
        .as_ref()
        .ok_or("Origin 300 needs an identity section")?;
    Ok(format!(
        "origin300,chassis-eeprom.0=,chassis-eeprom.1={},board-eeprom.0=,board-eeprom.1={},spd-eeprom.0=,spd-eeprom.1=,spd-eeprom.2=,spd-eeprom.3={},spd-eeprom.4=,spd-eeprom.5={}",
        qemu_path_option(&dir.join("state/io8-chassis.bin")),
        qemu_path_option(&dir.join("state/io8-board.bin")),
        qemu_path_option(&resolve(dir, &identity.spd_dimm2)),
        qemu_path_option(&resolve(dir, &identity.spd_dimm3)),
    ))
}

fn checksum(record: &mut [u8]) {
    let sum = record[..record.len() - 1]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    let last = record.len() - 1;
    record[last] = 0u8.wrapping_sub(sum);
}

fn chassis_record() -> [u8; 16] {
    let mut record = [0u8; 16];
    record[..11].copy_from_slice(&[
        0x00, 0x02, 0x00, 0xc2, b'N', b'A', 0xc2, b'N', b'A', 0xc1, 0x00,
    ]);
    checksum(&mut record);
    record
}

fn board_record(mac: &str) -> Result<[u8; 80]> {
    let mut digits = [0u8; 12];
    for (index, octet) in mac.split(':').enumerate() {
        let byte = u8::from_str_radix(octet, 16)?;
        digits[index * 2..index * 2 + 2].copy_from_slice(format!("{byte:02X}").as_bytes());
    }
    let mut record = [0u8; 80];
    let mut offset = 0;
    let prefix: &[u8] = b"\x00\x0a\x00\x00\x00\x00\xc9SYNTHETIC\xc3IO8\xc6000000\xccNOTOBSERVED0\x00\xc200\x01\x00\xc200";
    record[..prefix.len()].copy_from_slice(prefix);
    offset += prefix.len();
    for _ in 0..3 {
        record[offset] = 0x04;
        offset += 5;
    }
    debug_assert_eq!(offset, 64);
    record[64] = 0xcc;
    record[65..77].copy_from_slice(&digits);
    record[77] = 0xc1;
    checksum(&mut record);
    Ok(record)
}

fn seed_flash(prom: &[u8]) -> Result<Vec<u8>> {
    if prom.len() > 0x9e0000 {
        return Err("Origin 300 PROM overlaps the boot flash label".into());
    }
    let mut image = vec![0xff; FLASH_BYTES];
    image[..prom.len()].copy_from_slice(prom);
    image[0x9e0010..0x9e001c].copy_from_slice(b"PLOI\0\0\0\x01\0\0\0\x01");
    for (offset, name) in [
        (0x9e0100, b"DisableB".as_slice()),
        (0x9e0140, b"DisableD".as_slice()),
    ] {
        let record = &mut image[offset..offset + 64];
        record.fill(0);
        record[0] = 0x10;
        record[1..1 + name.len()].copy_from_slice(name);
        record[16..42].copy_from_slice(b"32: CPU failed early init.");
    }
    Ok(image)
}

pub fn prepare_state(dir: &Path, file: &MachineFile, prom: &Path) -> Result<()> {
    let identity = file
        .identity
        .as_ref()
        .ok_or("Origin 300 needs an identity section")?;
    let board = board_record(&identity.mac)?;
    fs::write(dir.join("state/io8-chassis.bin"), chassis_record())?;
    fs::write(dir.join("state/io8-board.bin"), board)?;
    let target = flash_path(dir);
    if target.exists() {
        let info = fs::symlink_metadata(&target)?;
        if !info.file_type().is_file() || info.len() != FLASH_BYTES as u64 {
            return Err(format!("invalid Origin 300 boot flash: {}", target.display()).into());
        }
        return Ok(());
    }
    let image = seed_flash(&fs::read(prom)?)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?
        .write_all(&image)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn io8_identity_has_reviewed_field_positions_and_checksum() {
        let board = board_record("02:00:5d:aa:bb:cc").unwrap();
        assert_eq!(board.len(), 80);
        assert_eq!(board[79], 0x0d);
        assert_eq!(board_record("08:00:69:12:34:56").unwrap()[79], 0x68);
        assert_eq!(&board[65..77], b"02005DAABBCC");
        assert_eq!(
            [board[49], board[54], board[59], board[64], board[77], board[78]],
            [4, 4, 4, 0xcc, 0xc1, 0]
        );
        assert_eq!(
            board.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );
        let chassis = chassis_record();
        assert_eq!(chassis.len(), 16);
        assert_eq!(
            chassis
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );
    }

    #[test]
    fn two_cpu_seed_marks_only_empty_slots() {
        let image = seed_flash(&vec![0x42; 1476264]).unwrap();
        assert_eq!(&image[0x9e0010..0x9e0014], b"PLOI");
        assert_eq!(&image[0x9e0101..0x9e0109], b"DisableB");
        assert_eq!(&image[0x9e0141..0x9e0149], b"DisableD");
        assert_eq!(image[0x9e0180], 0xff);
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(&image)),
            "ca02338573f873c8717b7a81ea5e28d148b0162da84e839e04747afe54617f50"
        );
    }
}
