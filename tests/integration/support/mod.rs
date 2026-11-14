//! Shared by the integration and acceptance tests: the product under test,
//! guests on a serial console, the prepared disk, and cleanup that survives
//! a failed test or a signal.
//!
//! The tests need a product tree (`bin/origami`, `libexec/origami/`) named by
//! `ORIGAMI_PRODUCT`, and a work directory named by `ORIGAMI_WORK`. They are
//! `#[ignore]`d so a plain `cargo test` stays fast; a run that asks for them
//! with `--ignored` and lacks either variable fails, never skips.
#![allow(dead_code)]

pub mod capture;
pub mod cleanup;
pub mod console;
pub mod disk;
pub mod guest;
pub mod product;

use std::time::Duration;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn pause(d: Duration) {
    std::thread::sleep(d);
}

/// The last `n` characters of `text`, cut on a character boundary.
pub fn tail(text: &str, n: usize) -> String {
    let skip = text.chars().count().saturating_sub(n);
    text.chars().skip(skip).collect()
}

/// The first `n` characters of `text`.
pub fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

pub fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// POSIX `cksum(1)`: CRC-32 with the length folded in, as `cksum` prints
/// "CRC LENGTH" on the guest.
pub fn posix_cksum(data: &[u8]) -> String {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = (i as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        *slot = c;
    }
    let mut crc = 0u32;
    for &b in data {
        crc = (crc << 8) ^ table[(((crc >> 24) as u8) ^ b) as usize];
    }
    let mut n = data.len();
    while n != 0 {
        crc = (crc << 8) ^ table[(((crc >> 24) as u8) ^ (n as u8)) as usize];
        n >>= 8;
    }
    format!("{} {}", !crc, data.len())
}

/// Pseudo-random bytes for test files. Not secret; it only has to differ
/// from run to run.
pub fn random_bytes(len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    getrandom::fill(&mut data).expect("system random source");
    data
}

#[cfg(test)]
mod tests {
    use super::posix_cksum;

    #[test]
    fn cksum_matches_the_posix_utility() {
        // `printf 123456789 | cksum` prints 930766865 9.
        assert_eq!(posix_cksum(b"123456789"), "930766865 9");
        // `: | cksum` of nothing prints 4294967295 0.
        assert_eq!(posix_cksum(b""), "4294967295 0");
    }
}
