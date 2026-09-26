use crate::{qemu_path_option, resolve, sha256_file, MachineFile, Origin300Identity, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DIMM2_SHA256: &str = "a8bb5857941fefae8e11037fe98b01d199199bd34eb059b269a8c4b6875ca0d3";
pub const DIMM3_SHA256: &str = "9e60c3400772d227b91c5d61cea8de7f89777d41377516de072944c26cef4aa7";
const FLASH_BYTES: usize = 16 * 1024 * 1024;
const FLASH_SEED_SHA256: &str = "6df02e413cc71206badcd375dfc53499e9fc6a193b76e3e37baca515e92b2f0f";
const PROM_SHA256: &str = "554924380189a03a512156af2faa38344902f251b49fd5c80c253c23ecaff27b";

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
    if offset != 64 {
        return Err("internal IO8 board layout error".into());
    }
    record[64] = 0xcc;
    record[65..77].copy_from_slice(&digits);
    record[77] = 0xc1;
    checksum(&mut record);
    Ok(record)
}

fn ensure_record(path: &Path, expected: &[u8]) -> Result<()> {
    if path.exists() {
        if fs::read(path)? != expected {
            return Err(format!(
                "existing Origin 300 identity differs from machine.toml: {}",
                path.display()
            )
            .into());
        }
        return Ok(());
    }
    let temporary = path.with_extension(format!(
        "bin.{}.{}.new",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(expected)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(windows)]
fn file_link_count(path: &Path) -> Result<u32> {
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct FileInformation {
        attributes: u32,
        created: FileTime,
        accessed: FileTime,
        modified: FileTime,
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            info: *mut FileInformation,
        ) -> i32;
    }
    let file = fs::File::open(path)?;
    let mut info = std::mem::MaybeUninit::<FileInformation>::uninit();
    // The Windows API writes the complete structure only on success.
    let success = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { info.assume_init().links })
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
    ensure_record(&dir.join("state/io8-chassis.bin"), &chassis_record())?;
    ensure_record(&dir.join("state/io8-board.bin"), &board)?;
    let target = flash_path(dir);
    if target.exists() {
        let info = fs::symlink_metadata(&target)?;
        if !info.file_type().is_file() || info.len() != FLASH_BYTES as u64 {
            return Err(format!("invalid Origin 300 boot flash: {}", target.display()).into());
        }
        #[cfg(unix)]
        if info.nlink() != 1 {
            return Err("Origin 300 boot flash must not share a hard link".into());
        }
        #[cfg(windows)]
        if file_link_count(&target)? != 1 {
            return Err("Origin 300 boot flash must not share a hard link".into());
        }
        return Ok(());
    }
    let prom_bytes = fs::read(prom)?;
    if format!("{:x}", Sha256::digest(&prom_bytes)) != PROM_SHA256 {
        return Err("Origin 300 PROM has an unexpected SHA-256".into());
    }
    let image = seed_flash(&prom_bytes)?;
    if format!("{:x}", Sha256::digest(&image)) != FLASH_SEED_SHA256 {
        return Err("Origin 300 boot flash seed differs from the reviewed image".into());
    }
    let temporary = dir.join(format!(
        "state/ip35-boot-flash.raw.{}.new",
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(&image)?;
        output.sync_all()?;
        fs::rename(&temporary, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sgi-{name}-{}", std::process::id()))
    }

    #[test]
    fn identity_write_recovers_from_an_incomplete_temporary_file() {
        let dir = test_dir("identity-retry");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("io8-board.bin");
        let abandoned = dir.join("io8-board.bin.0.old.new");
        fs::write(&abandoned, b"partial").unwrap();
        ensure_record(&path, b"complete").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete");
        assert_eq!(fs::read(&abandoned).unwrap(), b"partial");
        fs::write(&path, b"partial").unwrap();
        assert!(ensure_record(&path, b"complete").is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn linked_flash_is_rejected() {
        let dir = test_dir("linked-flash");
        fs::create_dir_all(dir.join("state")).unwrap();
        let target = flash_path(&dir);
        let other = dir.join("other.raw");
        fs::File::create(&other)
            .unwrap()
            .set_len(FLASH_BYTES as u64)
            .unwrap();
        fs::hard_link(&other, &target).unwrap();
        let file = MachineFile {
            format: 1,
            machine: crate::Machine {
                model: "origin300".into(),
                nodes: 1,
                cpus_per_node: 2,
                memory_per_node: "512MiB".into(),
                graphics: "none".into(),
            },
            firmware: crate::Firmware {
                image: String::new(),
            },
            identity: Some(Origin300Identity {
                mac: "08:00:69:12:34:56".into(),
                spd_dimm2: String::new(),
                spd_dimm3: String::new(),
            }),
            network: crate::Network::default(),
            drive: vec![],
        };
        let prom = dir.join("missing-prom");
        assert!(prepare_state(&dir, &file, &prom)
            .unwrap_err()
            .to_string()
            .contains("hard link"));
        fs::remove_dir_all(dir).unwrap();
    }

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
    }
}
