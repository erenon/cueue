use std::ffi::CString;
use std::io::{Error, ErrorKind};
use std::os::fd::{FromRawFd, OwnedFd};

/// Create a file descriptor that points to a location in memory.
pub(crate) unsafe fn memory_file() -> Result<OwnedFd, Error> {
    let name = CString::new("cueue").unwrap();
    let mem_fd = libc::memfd_create(name.as_ptr(), 0);
    if mem_fd < 0 {
        return Err(Error::new(ErrorKind::Other, "memfd_create"));
    }
    Ok(OwnedFd::from_raw_fd(mem_fd))
}

/// Platform specific flags that increase performance, but not required.
pub(super) fn platform_flags() -> i32 {
    libc::MAP_POPULATE
}
