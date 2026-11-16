//! Shared by the frame-plane tests: a fake-engine core in a temp library
//! with one created machine, seen through the control plane and the C ABI.
#![allow(dead_code)]

use origami_ffi::*;
use std::ffi::{c_char, CString};
use std::ptr;
use std::sync::Arc;

pub struct Lab {
    pub root: tempfile::TempDir,
    pub core: Arc<OrigamiCore>,
    pub c_core: *mut CoreHandle,
    pub key: String,
}

impl Lab {
    pub fn new(observer: Option<Arc<dyn OrigamiObserver>>) -> Lab {
        let root = tempfile::tempdir().unwrap();
        let core = OrigamiCore::new(root.path().to_str().unwrap().into(), observer).unwrap();
        let prom = root.path().join("prom.bin");
        std::fs::write(&prom, [0u8; 16]).unwrap();
        let op = core
            .create_machine(OrigamiCreateRequest {
                name: "m".into(),
                offering: "fake.1".into(),
                memory_per_node_mib: None,
                boot_prom: Some(prom.to_str().unwrap().into()),
                io_prom: None,
            })
            .unwrap();
        assert_eq!(op.wait(Some(10_000)), OrigamiOperationState::Succeeded);
        let key = op.result_machine().unwrap();
        let c_root = CString::new(root.path().to_str().unwrap()).unwrap();
        let mut c_core = ptr::null_mut();
        // SAFETY: valid NUL terminated string and out pointer.
        assert_eq!(
            unsafe { origami_core_new(c_root.as_ptr(), &mut c_core) },
            ORIGAMI_OK
        );
        Lab {
            root,
            core,
            c_core,
            key,
        }
    }

    /// The machine as the C plane opens it, started.
    pub fn c_machine(&self) -> *const origami_machine {
        let key = CString::new(self.key.as_str()).unwrap();
        let mut machine = ptr::null();
        // SAFETY: valid arguments.
        unsafe {
            assert_eq!(
                origami_machine_open(self.c_core, key.as_ptr(), &mut machine),
                ORIGAMI_OK
            );
            assert_eq!(origami_machine_start(machine), ORIGAMI_OK);
        }
        machine
    }

    pub fn advance(&self) {
        let key = CString::new(self.key.as_str()).unwrap();
        // SAFETY: valid arguments.
        assert_eq!(
            unsafe { origami_fake_advance_frame(self.c_core, key.as_ptr()) },
            ORIGAMI_OK
        );
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        // SAFETY: made by origami_core_new, released once.
        unsafe { origami_core_release(self.c_core) };
    }
}

pub fn last_error() -> String {
    let mut buf = [0 as c_char; 256];
    // SAFETY: `buf` is writable for its length.
    unsafe { origami_last_error(buf.as_mut_ptr(), buf.len()) };
    // SAFETY: NUL terminated by origami_last_error.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
