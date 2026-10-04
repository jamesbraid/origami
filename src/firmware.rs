use crate::{MachineFile, Offering, Result};
use std::ffi::{c_char, c_int, CString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[repr(C)]
struct FlashLayout {
    _opaque: [u8; 0],
}

#[repr(C)]
struct FlashImage {
    bytes: *mut u8,
    size: usize,
}

extern "C" {
    fn sgi_flash_layout_find(topology: *const c_char, role: *const c_char) -> *const FlashLayout;
    fn sgi_flash_layout_size(layout: *const FlashLayout) -> usize;
    fn sgi_flash_prepare(
        layout: *const FlashLayout,
        input: *const u8,
        size: usize,
        output: *mut FlashImage,
        error: *mut c_char,
        error_size: usize,
    ) -> c_int;
    fn sgi_flash_image_clear(image: *mut FlashImage);
}

impl Drop for FlashImage {
    fn drop(&mut self) {
        // The C library owns both success and partially allocated error results.
        unsafe { sgi_flash_image_clear(self) };
    }
}

pub struct Layout(*const FlashLayout);

impl Layout {
    pub fn find(topology: &str, role: &str) -> Result<Self> {
        let topology_c = CString::new(topology)?;
        let role_c = CString::new(role)?;
        // The generated C table owns immutable layouts for the process lifetime.
        let layout = unsafe { sgi_flash_layout_find(topology_c.as_ptr(), role_c.as_ptr()) };
        if layout.is_null() {
            return Err(format!("no supported {role} flash layout for {topology}").into());
        }
        Ok(Self(layout))
    }

    pub fn size(&self) -> usize {
        unsafe { sgi_flash_layout_size(self.0) }
    }

    pub fn prepare(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = FlashImage {
            bytes: std::ptr::null_mut(),
            size: 0,
        };
        let mut error = [0u8; 512];
        // input is borrowed for this call. output is cleared even if the call fails.
        let status = unsafe {
            sgi_flash_prepare(
                self.0,
                input.as_ptr(),
                input.len(),
                &mut output,
                error.as_mut_ptr().cast(),
                error.len(),
            )
        };
        if status != 0 {
            let end = error
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(error.len());
            return Err(format!(
                "cannot prepare firmware: {}",
                String::from_utf8_lossy(&error[..end])
            )
            .into());
        }
        if output.bytes.is_null() || output.size != self.size() {
            return Err("firmware library returned an invalid flash image".into());
        }
        // The library guarantees size readable bytes until clear. Copy into Rust ownership.
        Ok(unsafe { std::slice::from_raw_parts(output.bytes, output.size) }.to_vec())
    }

    pub fn read_original(&self, path: &Path) -> Result<Vec<u8>> {
        // Containers add headers. Bound reads before allocating or decoding user files.
        read_bounded(
            path,
            self.size()
                .checked_add(4096)
                .ok_or("invalid flash geometry")?,
        )
    }

    pub fn read_prepared(&self, path: &Path) -> Result<Vec<u8>> {
        let bytes = read_bounded(path, self.size())?;
        if bytes.len() != self.size() {
            return Err(format!(
                "prepared flash {} must contain exactly {} bytes",
                path.display(),
                self.size()
            )
            .into());
        }
        Ok(bytes)
    }
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let info = fs::metadata(path)
        .map_err(|error| format!("cannot read firmware {}: {error}", path.display()))?;
    if !info.is_file() {
        return Err(format!("firmware {} must be a regular file", path.display()).into());
    }
    let file = fs::File::open(path)
        .map_err(|error| format!("cannot read firmware {}: {error}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit as u64 {
        return Err(format!(
            "firmware {} must be a nonempty regular file no larger than {limit} bytes",
            path.display()
        )
        .into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err(format!("firmware {} changed size while reading", path.display()).into());
    }
    Ok(bytes)
}

pub fn cpu_paths(dir: &Path, offering: &Offering) -> Vec<PathBuf> {
    (0..offering.nodes)
        .map(|node| match offering.firmware.kind.as_str() {
            "ip35-prom" if node == 0 => dir.join("state/ip35-boot-flash.raw"),
            "ip35-prom" => dir.join(format!("state/ip35-boot-flash{node}.raw")),
            "ip30-prom" => dir.join("state/cpu-flash.raw"),
            _ => dir.join(format!("state/node-proms/node{}.bin", node + 1)),
        })
        .collect()
}

pub fn io_paths(dir: &Path, file: &MachineFile, offering: &Offering) -> Vec<PathBuf> {
    if file.firmware.io_image.is_none() {
        return vec![];
    }
    (0..io_count(offering))
        .map(|index| dir.join(format!("state/io-proms/io{index}.bin")))
        .collect()
}

pub fn io_count(offering: &Offering) -> usize {
    offering
        .resources
        .iter()
        .filter(|resource| resource.kind == "io-flash")
        .map(|resource| resource.count as usize)
        .sum()
}

pub fn io_backend_ids(offering: &Offering) -> Result<Vec<&str>> {
    let mut ids = Vec::new();
    for resource in offering
        .resources
        .iter()
        .filter(|resource| resource.kind == "io-flash")
    {
        if resource.backend_ids.len() != resource.count as usize {
            return Err(format!(
                "invalid IO flash backend metadata for {}",
                offering.topology
            )
            .into());
        }
        ids.extend(resource.backend_ids.iter().map(String::as_str));
    }
    Ok(ids)
}
