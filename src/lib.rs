
//! A high performance, single-producer, single-consumer, bounded circular buffer
//! of contiguous bytes, that supports lock-free atomic batch operations,
//! suitable for inter-thread communication.
//!
//!```
//!     let (mut w, mut r) = cueue::cueue(1 << 20).unwrap();
//! 
//!     w.begin_write();
//!     assert!(w.write_capacity() >= 9);
//!     w.write(b"foo");
//!     w.write(b"bar");
//!     w.write(b"baz");
//!     w.end_write();
//! 
//!     let read_result = r.begin_read();
//!     assert_eq!(read_result, b"foobarbaz");
//!     r.end_read();
//! ```

use std::ffi::CString;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::Ordering;

use libc::{c_void, ftruncate, mmap, munmap, sysconf};
use libc::{
    MAP_ANONYMOUS, MAP_FAILED, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, PROT_READ, PROT_WRITE,
    _SC_PAGESIZE,
};

/// Wraps POSIX C errno with an additional hint.
///
/// The hint is used to identify the opration that triggered the error.
pub struct CError {
    hint: &'static str,
    err: std::io::Error,
}

impl CError {
    /// Create a new CError from the given hint and the current errno.
    fn new(hint: &'static str) -> Self {
        Self {
            hint,
            err: std::io::Error::last_os_error(),
        }
    }
}

impl std::fmt::Debug for CError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.hint, self.err)
    }
}

/// Create a file descriptor that points to a location in memory.
#[cfg(target_os = "linux")]
unsafe fn memoryfile() -> Result<OwnedFd, CError> {
    let name = CString::new("cueue").unwrap();
    let memfd = libc::memfd_create(name.as_ptr(), 0);
    if memfd < 0 {
        return Err(CError::new("memfd_create"));
    }
    Ok(OwnedFd::from_raw_fd(memfd))
}

#[cfg(target_os = "macos")]
unsafe fn memoryfile() -> Result<OwnedFd, CError> {
    let path = CString::new("/tmp/cueue_XXXXXX").unwrap();
    let path_cstr = path.into_raw();
    let tmpfd = libc::mkstemp(path_cstr);
    let path = CString::from_raw(path_cstr);
    if tmpfd < 0 {
        return Err(CError::new("mkstemp"));
    }
    let memfd = libc::shm_open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_EXCL);
    libc::unlink(path.as_ptr());
    libc::close(tmpfd);
    if memfd < 0 {
        return Err(CError::new("shm_open"));
    }

    Ok(OwnedFd::from_raw_fd(memfd))
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

    fn ptr(&mut self) -> *mut u8 {
        self.map as *mut u8
    }
}

impl Drop for MemoryMap {
    fn drop(&mut self) {
        if !self.failed() {
            unsafe {
                munmap(self.map, self.size);
            }
        }
    }
}

/// Platform specific flags that increase performance, but not required.
#[cfg(target_os = "linux")]
fn platform_flags() -> i32 {
    libc::MAP_POPULATE
}

#[cfg(target_os = "macos")]
fn platform_flags() -> i32 {
    0
}

/// Map a `size` chunk of `fd` at `offset` twice, next to each other in virtual memory
/// The size of the file pointed by `fd` must be >= offset + size.
unsafe fn doublemap(fd: RawFd, offset: usize, size: usize) -> Result<MemoryMap, CError> {
    // Create a map, offset + twice the size, to get a suitable virtual address which will work with MAP_FIXED
    let rw = PROT_READ | PROT_WRITE;
    let mapsize = offset + size * 2;
    let mut map = MemoryMap::new(
        mmap(
            std::ptr::null_mut(),
            mapsize,
            rw,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        ),
        mapsize,
    );
    if map.failed() {
        return Err(CError::new("mmap 1"));
    }

    // Map f twice, put maps next to each other with MAP_FIXED
    // MAP_SHARED is required to have the changes propagated between maps
    let first_addr = map.ptr().add(offset) as *mut c_void;
    let first_map = mmap(
        first_addr,
        size,
        rw,
        MAP_SHARED | MAP_FIXED | platform_flags(),
        fd,
        offset as i64,
    );
    if first_map != first_addr {
        return Err(CError::new("mmap 2"));
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
        return Err(CError::new("mmap 3"));
    }

    // man mmap:
    // If the memory region specified by addr and len overlaps
    // pages of any existing mapping(s), then the overlapped part
    // of the existing mapping(s) will be discarded.
    // -> No need to munmap `first_map` and `second_map`, drop(map) will do both

    Ok(map)
}

/// Returns smallest power of 2 not smaller than `n`
fn next_power_two(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }

    n -= 1;
    let mut result = 1;
    while n != 0 {
        n >>= 1;
        result <<= 1;
    }
    result
}

/// Force an AtomicU64 to a separate cache-line to avoid false-sharing.
/// This wrapper is needed as I was unable to specify alignment for individual fields.
#[repr(align(64))]
#[derive(Default)]
struct CacheLineAlignedAU64(std::sync::atomic::AtomicU64);

/// The shared metadata of a Cueue.
///
/// Cueue is empty if R == W
/// Cueue is full if W == R+capacity
/// Invariant: W >= R
/// Invariant: R + capacity >= W
#[derive(Default)]
struct ControlBlock {
    write_position: CacheLineAlignedAU64,
    read_position: CacheLineAlignedAU64,
}

/// Writer of a Cueue.
///
/// See examples/ for usage.
pub struct Writer<'a> {
    _mem: std::sync::Arc<MemoryMap>,
    cb: &'a ControlBlock,
    mask: u64,

    buffer: *mut u8,
    write_begin: *mut u8,
    write_pos: *mut u8,
    write_end: *mut u8,
}

impl<'a> Writer<'a> {
    fn new(
        mem: std::sync::Arc<MemoryMap>,
        cb: &'a ControlBlock,
        buffer: *mut u8,
        capacity: usize,
    ) -> Self {
        Self {
            _mem: mem,
            cb,
            mask: capacity as u64 - 1,
            buffer,
            write_begin: std::ptr::null_mut(),
            write_pos: std::ptr::null_mut(),
            write_end: std::ptr::null_mut(),
        }
    }

    /// Maximum number of bytes the referenced `cueue` can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        (self.mask + 1) as usize
    }

    /// Maximum number of bytes that can be yet written
    /// before end_write + begin_write need to be called again.
    #[inline]
    pub fn write_capacity(&self) -> usize {
        unsafe { self.write_end.offset_from(self.write_pos) as usize }
    }

    /// Maximize `write_capacity()`.
    ///
    /// Resets the internal write buffer, uncommitted writes
    /// (those without subsequent end_write) will be lost.
    ///
    /// Returns `write_capacity()`
    pub fn begin_write(&mut self) -> usize {
        let w = self.cb.write_position.0.load(Ordering::Relaxed);
        let r = self.cb.read_position.0.load(Ordering::Acquire);

        debug_assert!(r <= w);
        debug_assert!(r + self.capacity() as u64 >= w);

        let wc = self.capacity() as u64 - (w.wrapping_sub(r));
        let wi = w & self.mask;

        self.write_begin = unsafe { self.buffer.offset(wi as isize) };
        self.write_pos = self.write_begin;
        self.write_end = unsafe { self.write_begin.offset(wc as isize) };

        wc as usize
    }

    /// Attempt to satisfy `write_capacity() >= size`.
    ///
    /// Possibly calls begin_write, uncommitted writes
    /// (those without subsequent end_write) will be lost.
    ///
    /// Returns `size <= write_capacity()`
    #[inline]
    pub fn begin_write_if_needed(&mut self, size: usize) -> bool {
        if size <= self.write_capacity() {
            true
        } else {
            size <= self.begin_write()
        }
    }

    /// Copy `src` to the internal write buffer.
    ///
    /// Requires `write_capacity() >= src.len()`.
    #[inline]
    pub fn write(&mut self, src: &[u8]) {
        debug_assert!(self.write_capacity() >= src.len());

        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), self.write_pos, src.len());
            self.write_pos = self.write_pos.add(src.len());
        }
    }

    /// Make the written parts of the internal write buffer available for reading.
    pub fn end_write(&mut self) {
        let w = self.cb.write_position.0.load(Ordering::Relaxed);
        unsafe {
            let write_size = self.write_pos.offset_from(self.write_begin);
            self.write_begin = self.write_begin.offset(write_size);
            self.cb
                .write_position
                .0
                .store(w + write_size as u64, Ordering::Release);
        }
    }
}

unsafe impl<'a> Send for Writer<'a> {}

/// Reader of a Cueue.
///
/// See examples/ for usage.
pub struct Reader<'a> {
    _mem: std::sync::Arc<MemoryMap>,
    cb: &'a ControlBlock,
    mask: u64,

    buffer: *const u8,
    read_begin: *const u8,
    read_size: u64,
}

impl<'a> Reader<'a> {
    fn new(
        mem: std::sync::Arc<MemoryMap>,
        cb: &'a ControlBlock,
        buffer: *const u8,
        capacity: usize,
    ) -> Self {
        Self {
            _mem: mem,
            cb,
            mask: capacity as u64 - 1,
            buffer,
            read_begin: std::ptr::null(),
            read_size: 0,
        }
    }

    /// Maximum number of bytes the referenced `cueue` can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        (self.mask + 1) as usize
    }

    /// Return a slice of bytes written and committed by the Writer.
    pub fn begin_read(&mut self) -> &'a [u8] {
        let w = self.cb.write_position.0.load(Ordering::Acquire);
        let r = self.cb.read_position.0.load(Ordering::Relaxed);

        debug_assert!(r <= w);
        debug_assert!(r + self.capacity() as u64 >= w);

        let ri = r & self.mask;

        self.read_begin = unsafe { self.buffer.offset(ri as isize) };
        self.read_size = w - r;

        unsafe { std::slice::from_raw_parts(self.read_begin, self.read_size as usize) }
    }

    /// Mark the slice previously acquired by `begin_read` as consumed,
    /// making it available for writing.
    pub fn end_read(&mut self) {
        let r = self.cb.read_position.0.load(Ordering::Relaxed);
        self.cb
            .read_position
            .0
            .store(r + self.read_size, Ordering::Release);
    }
}

unsafe impl<'a> Send for Reader<'a> {}

/// Create a single-producer, single-consumer `Cueue`.
///
/// The `requested_capacity` is a lower bound of the actual capacity
/// of the constructed queue: it might be rounded up to match system requirements
/// (power of two, multiple of page size).
///
/// On success, returns a `(Writer, Reader)` pair, that share the ownership
/// of the underlying circular byte array.
pub fn cueue<'a>(requested_capacity: usize) -> Result<(Writer<'a>, Reader<'a>), CError> {
    let pagesize = unsafe { sysconf(_SC_PAGESIZE) as usize };
    let capacity = next_power_two(usize::max(requested_capacity, pagesize));
    let cbsize = pagesize;

    let (mut map, cb) = unsafe {
        let f = memoryfile()?;
        if ftruncate(f.as_raw_fd(), (cbsize + capacity) as i64) != 0 {
            return Err(CError::new("ftruncate"));
        }
        let mut map = doublemap(f.as_raw_fd(), cbsize, capacity)?;

        let cbp = map.ptr() as *mut ControlBlock;
        cbp.write(ControlBlock::default());
        (map, &mut *cbp)
    };
    let buffer = unsafe { map.ptr().add(cbsize) };
    let shared_map = std::sync::Arc::new(map);

    Ok((
        Writer::new(shared_map.clone(), cb, buffer, capacity),
        Reader::new(shared_map, cb, buffer, capacity),
    ))
}

#[cfg(test)]
mod tests;
