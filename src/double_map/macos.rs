use std::ffi::CString;
use std::os::fd::{FromRawFd, OwnedFd};

use std::io::{Error, ErrorKind};

pub(crate) unsafe fn memory_file() -> Result<OwnedFd, Error> {
    let path = CString::new("/tmp/cueue_XXXXXX").unwrap();
    let path_cstr = path.into_raw();
    let tmp_fd = libc::mkstemp(path_cstr);
    let path = CString::from_raw(path_cstr);
    if tmp_fd < 0 {
        return Err(Error::new(ErrorKind::Other, "mkstemp"));
    }
    let mem_fd = libc::shm_open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_EXCL);
    libc::unlink(path.as_ptr());
    libc::close(tmp_fd);
    if mem_fd < 0 {
        return Err(Error::new(ErrorKind::Other, "shm_open"));
    }

    Ok(OwnedFd::from_raw_fd(mem_fd))
}

/// Platform specific flags that increase performance, but not required.
pub(super) const fn platform_flags() -> i32 {
    0
}
