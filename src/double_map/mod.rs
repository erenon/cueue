use libc::{
    c_void, mmap, MAP_ANONYMOUS, MAP_FAILED, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, PROT_READ,
    PROT_WRITE, _SC_PAGESIZE,
};
use std::io::{Error, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::io::RawFd;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod os;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod os;

pub(crate) use os::memory_file;

/// Map a `size` chunk of `fd` at `offset` twice, next to each other in virtual memory
/// The size of the file pointed by `fd` must be >= offset + size.
unsafe fn double_map(fd: RawFd, offset: usize, size: usize) -> Result<MemoryMap, Error> {
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

/// A chunk of memory allocated using mmap.
///
/// Deallocates the memory on Drop.
struct MemoryMap {
    map: *mut c_void,
    size: usize,
}

impl MemoryMap {
    fn new(map: *mut c_void, size: usize) -> Self {
        Self { map, size }
    }

    fn failed(&self) -> bool {
        self.map == MAP_FAILED
    }

    fn ptr(&self) -> *mut u8 {
        self.map as *mut u8
    }
}

impl Drop for MemoryMap {
    fn drop(&mut self) {
        if !self.failed() {
            unsafe { libc::munmap(self.map, self.size) };
        }
    }
}

pub(crate) struct MemoryMapInitialized<T> {
    map: MemoryMap,
    buf: *mut T,
    cap: usize,
}

impl<T> MemoryMapInitialized<T>
where
    T: Default,
{
    pub(crate) fn new(requested_capacity: usize) -> Result<Self, Error> {
        let pagesize = unsafe { libc::sysconf(_SC_PAGESIZE) as usize };
        let capacity = requested_capacity.max(pagesize).next_power_of_two();
        let cb_size = pagesize;

        unsafe {
            let f = memory_file()?;
            let buf_size = capacity * std::mem::size_of::<T>();
            if libc::ftruncate(f.as_raw_fd(), (cb_size + buf_size) as i64) != 0 {
                return Err(Error::new(ErrorKind::Other, "ftruncate"));
            }
            let map = double_map(f.as_raw_fd(), cb_size, buf_size)?;

            // default initialize elements.
            // this is required to make sure writer always sees initialized elements
            let buffer = map.ptr().add(cb_size).cast::<T>();
            let init_map = MemoryMapInitialized::construct(map, buffer, capacity);

            Ok(init_map)
        }
    }

    fn construct(map: MemoryMap, buf: *mut T, cap: usize) -> Self {
        for i in 0..cap {
            unsafe {
                buf.add(i).write(T::default());
            }
        }
        Self { map, buf, cap }
    }

    /// Returns capacity of the buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Returns a page pointer for control block.
    #[inline]
    pub fn control_block(&self) -> *mut u8 {
        self.map.ptr()
    }

    /// Returns buffer pointer.
    #[inline]
    pub fn buffer(&self) -> *mut T {
        self.buf
    }
}

impl<T> Drop for MemoryMapInitialized<T> {
    fn drop(&mut self) {
        for i in 0..self.cap {
            unsafe {
                self.buf.add(i).drop_in_place();
            }
        }
    }
}
