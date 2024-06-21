use libc::{
    c_void, mmap, MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, PROT_READ, PROT_WRITE,
};
use std::io::{Error, ErrorKind};
use std::os::unix::io::RawFd;

use crate::MemoryMap;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod os;

pub(crate) use os::memory_file;

/// Map a `size` chunk of `fd` at `offset` twice, next to each other in virtual memory
/// The size of the file pointed by `fd` must be >= offset + size.
pub(crate) unsafe fn double_map(fd: RawFd, offset: usize, size: usize) -> Result<MemoryMap, Error> {
    // Create a map, offset + twice the size, to get a suitable virtual address which will work with MAP_FIXED
    let rw = PROT_READ | PROT_WRITE;
    let map_size = offset + size * 2;
    let map = MemoryMap::new(
        mmap(
            std::ptr::null_mut(),
            map_size,
            rw,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        ),
        map_size,
    );
    if map.failed() {
        return Err(Error::new(ErrorKind::Other, "mmap 1"));
    }

    // Map f twice, put maps next to each other with MAP_FIXED
    // MAP_SHARED is required to have the changes propagated between maps
    let first_addr = map.ptr().add(offset) as *mut c_void;
    let first_map = mmap(
        first_addr,
        size,
        rw,
        MAP_SHARED | MAP_FIXED | os::platform_flags(),
        fd,
        offset as i64,
    );
    if first_map != first_addr {
        return Err(Error::new(ErrorKind::Other, "mmap 2"));
    }

    let second_addr = map.ptr().add(offset + size) as *mut c_void;
    let second_map = mmap(
        second_addr,
        size,
        rw,
        MAP_SHARED | MAP_FIXED,
        fd,
        offset as i64,
    );
    if second_map != second_addr {
        return Err(Error::new(ErrorKind::Other, "mmap 3"));
    }

    // man mmap:
    // If the memory region specified by addr and len overlaps
    // pages of any existing mapping(s), then the overlapped part
    // of the existing mapping(s) will be discarded.
    // -> No need to munmap `first_map` and `second_map`, drop(map) will do both

    Ok(map)
}
