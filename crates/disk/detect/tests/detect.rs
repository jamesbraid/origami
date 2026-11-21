use disk_detect::{detect, Detected};
use std::io::Read;
use std::sync::Arc;

/// An SGI volume header whose slot 0 is a Linux partition holding a small
/// ext2 file system with an os-release naming Debian 12.
fn linux_disk() -> Vec<u8> {
    let mut ext2 = vec![];
    flate2::read::GzDecoder::new(&include_bytes!("data/ext2.img.gz")[..])
        .read_to_end(&mut ext2)
        .unwrap();
    let mut disk = vec![0u8; 8 * 512];
    disk.extend(&ext2);
    disk[0..4].copy_from_slice(&0x0be5a941u32.to_be_bytes());
    let p = 312;
    disk[p..p + 4].copy_from_slice(&((ext2.len() / 512) as u32).to_be_bytes());
    disk[p + 4..p + 8].copy_from_slice(&8u32.to_be_bytes());
    disk[p + 8..p + 12].copy_from_slice(&0x83u32.to_be_bytes());
    disk
}

#[test]
fn linux_partition_reports_os_release() {
    let disk = linux_disk();
    let size = disk.len() as u64;
    let got = detect(&(Arc::new(disk) as _), size);
    assert_eq!(
        got,
        Detected::Linux {
            name: "Debian GNU/Linux".into(),
            version: Some("12".into())
        }
    );
    assert_eq!(got.to_string(), "Debian GNU/Linux 12");
}

fn gunzip(gz: &[u8]) -> Vec<u8> {
    let mut v = vec![];
    flate2::read::GzDecoder::new(gz)
        .read_to_end(&mut v)
        .unwrap();
    v
}

fn detect_bytes(disk: Vec<u8>) -> String {
    let size = disk.len() as u64;
    detect(&(Arc::new(disk) as _), size).to_string()
}

#[test]
fn bare_ext_file_system_is_detected() {
    assert_eq!(
        detect_bytes(gunzip(include_bytes!("data/ext2.img.gz"))),
        "Debian GNU/Linux 12"
    );
}

#[test]
fn a_panicking_reader_is_unknown() {
    struct Boom;
    impl disk_rofs::ReadAt for Boom {
        fn read_at(&self, _: u64, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("boom")
        }
    }
    assert_eq!(detect(&(Arc::new(Boom) as _), 1 << 20), Detected::Unknown);
}

#[test]
fn blank_disk_is_unknown() {
    assert_eq!(
        detect(&(Arc::new(vec![0u8; 1 << 16]) as _), 1 << 16),
        Detected::Unknown
    );
}

/// Real published disks, named by environment variables; skipped otherwise.
fn real(var: &str, want: &str) {
    let Ok(path) = std::env::var(var) else { return };
    assert_eq!(
        disk_detect::detect_path(path.as_ref()).unwrap().to_string(),
        want
    );
}

#[test]
#[ignore = "needs DISK_DETECT_IRIX53 pointing at a published IRIX 5.3 CHD"]
fn irix_53_chd() {
    real("DISK_DETECT_IRIX53", "IRIX 5.3");
}

#[test]
#[ignore = "needs DISK_DETECT_IRIX65 pointing at a published IRIX 6.5.22 CHD"]
fn irix_65_chd() {
    real("DISK_DETECT_IRIX65", "IRIX 6.5.22m");
}

#[test]
#[ignore = "needs DISK_DETECT_GENTOO pointing at a bare ext4 Gentoo file system"]
fn gentoo_ext4_image() {
    real("DISK_DETECT_GENTOO", "Gentoo 2.18");
}
