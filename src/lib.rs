//! A high performance, single-producer, single-consumer, bounded circular buffer
//! of contiguous elements, that supports lock-free atomic batch operations,
//! suitable for inter-thread communication.
//!
//!```
//! let (mut w, mut r) = cueue::cueue(1 << 20).unwrap();
//!
//! let buf = w.write_chunk();
//! assert!(buf.len() >= 9);
//! buf[..9].copy_from_slice(b"foobarbaz");
//! w.commit(9);
//!
//! let read_result = r.read_chunk();
//! assert_eq!(read_result, b"foobarbaz");
//! r.commit();
//!```
//!
//! Elements in the queue are always initialized, and not dropped until the queue is dropped.
//! This allows re-use of elements (useful for elements with heap allocated contents),
//! and prevents contention on the senders heap (by avoiding the consumer freeing memory
//! the sender allocated).

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::Ordering;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use libc::{c_void, ftruncate, mmap, munmap, sysconf};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use libc::{
    MAP_ANONYMOUS, MAP_FAILED, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, PROT_READ, PROT_WRITE,
    _SC_PAGESIZE,
};

fn errno_with_hint(hint: &str) -> std::io::Error {
    std::io::Error::new(std::io::Error::last_os_error().kind(), hint)
}

/// Create a file descriptor that points to a location in memory.
#[cfg(target_os = "linux")]
unsafe fn memoryfile() -> Result<OwnedFd, std::io::Error> {
    let name = CString::new("cueue").unwrap();
    let memfd = libc::memfd_create(name.as_ptr(), 0);
    if memfd < 0 {
        return Err(errno_with_hint("memfd_create"));
    }
    Ok(OwnedFd::from_raw_fd(memfd))
}

#[cfg(target_os = "macos")]
unsafe fn memoryfile() -> Result<OwnedFd, std::io::Error> {
    let path = CString::new("/tmp/cueue_XXXXXX").unwrap();
    let path_cstr = path.into_raw();
    let tmpfd = libc::mkstemp(path_cstr);
    let path = CString::from_raw(path_cstr);
    if tmpfd < 0 {
        return Err(errno_with_hint("mkstemp"));
    }
    let memfd = libc::shm_open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_EXCL);
    libc::unlink(path.as_ptr());
    libc::close(tmpfd);
    if memfd < 0 {
        return Err(errno_with_hint("shm_open"));
    }

    Ok(OwnedFd::from_raw_fd(memfd))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
unsafe fn memoryfile() {
    todo!("Only Linux and macOS are supported so far");
}

/// A chunk of memory allocated using mmap.
///
/// Deallocates the memory on Drop.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct MemoryMap {
    map: *mut c_void,
    size: usize,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for MemoryMap {
    fn drop(&mut self) {
        if !self.failed() {
            unsafe {
                munmap(self.map, self.size);
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct MemoryMap {}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl MemoryMap {
    fn ptr(&self) -> *mut u8 {
        todo!("Only Linux and macOS are supported so far");
    }
}

struct MemoryMapInitialized<T> {
    map: MemoryMap,
    buf: *mut T,
    cap: usize,
}

impl<T> MemoryMapInitialized<T>
where
    T: Default,
{
    fn new(map: MemoryMap, buf: *mut T, cap: usize) -> Self {
        for i in 0..cap {
            unsafe {
                buf.add(i).write(T::default());
            }
        }
        Self { map, buf, cap }
    }

    #[inline]
    fn controlblock(&self) -> *mut ControlBlock {
        self.map.ptr().cast::<ControlBlock>()
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

/// Platform specific flags that increase performance, but not required.
#[cfg(target_os = "linux")]
fn platform_flags() -> i32 {
    libc::MAP_POPULATE
}

#[cfg(not(target_os = "linux"))]
fn platform_flags() -> i32 {
    0
}

/// Map a `size` chunk of `fd` at `offset` twice, next to each other in virtual memory
/// The size of the file pointed by `fd` must be >= offset + size.
#[cfg(any(target_os = "linux", target_os = "macos"))]
unsafe fn doublemap(fd: RawFd, offset: usize, size: usize) -> Result<MemoryMap, std::io::Error> {
    // Create a map, offset + twice the size, to get a suitable virtual address which will work with MAP_FIXED
    let rw = PROT_READ | PROT_WRITE;
    let mapsize = offset + size * 2;
    let map = MemoryMap::new(
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
        return Err(errno_with_hint("mmap 1"));
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
        return Err(errno_with_hint("mmap 2"));
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
        return Err(errno_with_hint("mmap 3"));
    }

    // man mmap:
    // If the memory region specified by addr and len overlaps
    // pages of any existing mapping(s), then the overlapped part
    // of the existing mapping(s) will be discarded.
    // -> No need to munmap `first_map` and `second_map`, drop(map) will do both

    Ok(map)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
unsafe fn doublemap() {
    todo!("Only Linux and macOS are supported so far");
}

/// Returns smallest power of 2 not smaller than `n`,
/// or an error if the expected result cannot be represented by the return type.
fn next_power_two(n: usize) -> Result<usize, std::io::Error> {
    if n == 0 {
        return Ok(1);
    }

    let mut m = n - 1;
    let mut result = 1;
    while m != 0 {
        m >>= 1;
        result <<= 1;
    }

    if result >= n {
        Ok(result)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "next_power_two",
        ))
    }
}

/// Force an AtomicU64 to a separate cache-line to avoid false-sharing.
/// This wrapper is needed as I was unable to specify alignment for individual fields.
#[repr(align(128))]
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
pub struct Writer<T> {
    mem: std::sync::Arc<MemoryMapInitialized<T>>,
    cb: *mut ControlBlock,
    mask: u64,

    buffer: *mut T,
    write_begin: *mut T,
    write_capacity: usize,
}

impl<T> Writer<T>
where
    T: Default,
{
    fn new(mem: std::sync::Arc<MemoryMapInitialized<T>>, buffer: *mut T, capacity: usize) -> Self {
        let cb = mem.controlblock();
        Self {
            mem,
            cb,
            mask: capacity as u64 - 1,
            buffer,
            write_begin: std::ptr::null_mut(),
            write_capacity: 0,
        }
    }

    /// Maximum number of elements the referenced `cueue` can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        (self.mask + 1) as usize
    }

    /// Get a writable slice of maximum available size.
    ///
    /// The elements in the returned slice are either default initialized
    /// (never written yet) or are the result of previous writes.
    /// The writer is free to overwrite or reuse them.
    ///
    /// After write, `commit` must be called, to make the written elements
    /// available for reading.
    pub fn write_chunk(&mut self) -> &mut [T] {
        let w = self.write_pos().load(Ordering::Relaxed);
        let r = self.read_pos().load(Ordering::Acquire);

        debug_assert!(r <= w);
        debug_assert!(r + self.capacity() as u64 >= w);

        let wi = w & self.mask;
        self.write_capacity = (self.capacity() as u64 - (w.wrapping_sub(r))) as usize;

        unsafe {
            self.write_begin = self.buffer.offset(wi as isize);
            std::slice::from_raw_parts_mut(self.write_begin, self.write_capacity)
        }
    }

    /// Make `n` number of elements, written to the slice returned by `write_chunk`
    /// available for reading.
    ///
    /// `n` is checked: if too large, gets truncated to the maximum committable size.
    ///
    /// Returns the number of committed elements.
    pub fn commit(&mut self, n: usize) -> usize {
        let m = usize::min(self.write_capacity, n);
        unsafe {
            self.unchecked_commit(m);
        }
        m
    }

    unsafe fn unchecked_commit(&mut self, n: usize) {
        let w = self.write_pos().load(Ordering::Relaxed);
        self.write_begin = self.write_begin.add(n);
        self.write_capacity -= n;
        self.write_pos().store(w + n as u64, Ordering::Release);
    }

    /// Returns true, if the Reader counterpart was dropped.
    pub fn is_abandoned(&self) -> bool {
        std::sync::Arc::strong_count(&self.mem) < 2
    }

    /// Write and commit a single element, or return it if the queue was full.
    pub fn push(&mut self, t: T) -> Result<(), T> {
        let chunk = self.write_chunk();
        if !chunk.is_empty() {
            chunk[0] = t;
            self.commit(1);
            Ok(())
        } else {
            Err(t)
        }
    }

    #[inline]
    fn write_pos(&self) -> &std::sync::atomic::AtomicU64 {
        unsafe { &(*self.cb).write_position.0 }
    }

    #[inline]
    fn read_pos(&self) -> &std::sync::atomic::AtomicU64 {
        unsafe { &(*self.cb).read_position.0 }
    }
}

unsafe impl<T> Send for Writer<T> {}

/// Reader of a Cueue.
///
/// See examples/ for usage.
pub struct Reader<T> {
    mem: std::sync::Arc<MemoryMapInitialized<T>>,
    cb: *mut ControlBlock,
    mask: u64,

    buffer: *const T,
    read_begin: *const T,
    read_size: u64,
}

impl<T> Reader<T>
where
    T: Default,
{
    fn new(
        mem: std::sync::Arc<MemoryMapInitialized<T>>,
        buffer: *const T,
        capacity: usize,
    ) -> Self {
        let cb = mem.controlblock();
        Self {
            mem,
            cb,
            mask: capacity as u64 - 1,
            buffer,
            read_begin: std::ptr::null(),
            read_size: 0,
        }
    }

    /// Maximum number of elements the referenced `cueue` can hold.
    #[inline]
    pub fn capacity(&self) -> usize {
        (self.mask + 1) as usize
    }

    /// Return a slice of elements written and committed by the Writer.
    pub fn read_chunk(&mut self) -> &[T] {
        let w = self.write_pos().load(Ordering::Acquire);
        let r = self.read_pos().load(Ordering::Relaxed);

        debug_assert!(r <= w);
        debug_assert!(r + self.capacity() as u64 >= w);

        let ri = r & self.mask;

        self.read_size = w - r;

        unsafe {
            self.read_begin = self.buffer.offset(ri as isize);
            std::slice::from_raw_parts(self.read_begin, self.read_size as usize)
        }
    }

    /// Return a slice of elements written and committed by the Writer.
    ///
    /// The length of the returned slice will be less or equal than `n`.
    pub fn limited_read_chunk(&mut self, n: u64) -> &[T] {
        let w = self.write_pos().load(Ordering::Acquire);
        let r = self.read_pos().load(Ordering::Relaxed);

        debug_assert!(r <= w);
        debug_assert!(r + self.capacity() as u64 >= w);

        let ri = r & self.mask;
        let rs = n.min(w - r);

        self.read_size = rs;

        unsafe {
            self.read_begin = self.buffer.add(ri as usize);
            std::slice::from_raw_parts(self.read_begin, self.read_size as usize)
        }
    }

    /// Mark the slice previously acquired by `read_chunk` as consumed,
    /// making it available for writing.
    pub fn commit(&mut self) {
        let r = self.read_pos().load(Ordering::Relaxed);
        let rs = self.read_size;
        self.read_pos().store(r + rs, Ordering::Release);
    }

    /// Returns true, if the Writer counterpart was dropped.
    pub fn is_abandoned(&self) -> bool {
        std::sync::Arc::strong_count(&self.mem) < 2
    }

    #[inline]
    fn write_pos(&self) -> &std::sync::atomic::AtomicU64 {
        unsafe { &(*self.cb).write_position.0 }
    }

    #[inline]
    fn read_pos(&self) -> &std::sync::atomic::AtomicU64 {
        unsafe { &(*self.cb).read_position.0 }
    }
}

unsafe impl<T> Send for Reader<T> {}

/// Create a single-producer, single-consumer `Cueue`.
///
/// The `requested_capacity` is a lower bound of the actual capacity
/// of the constructed queue: it might be rounded up to match system requirements
/// (power of two, multiple of page size).
///
/// `requested_capacity` must not be bigger than 2^63.
///
/// On success, returns a `(Writer, Reader)` pair, that share the ownership
/// of the underlying circular array.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn cueue<T>(requested_capacity: usize) -> Result<(Writer<T>, Reader<T>), std::io::Error>
where
    T: Default,
{
    let pagesize = unsafe { sysconf(_SC_PAGESIZE) as usize };
    let capacity = next_power_two(usize::max(requested_capacity, pagesize))?;
    let cbsize = pagesize;

    if std::mem::size_of::<ControlBlock>() > pagesize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "ControlBlock does not fit in a single page",
        ));
    }

    let (initmap, buffer) = unsafe {
        let f = memoryfile()?;
        let bufsize = capacity * std::mem::size_of::<T>();
        if ftruncate(f.as_raw_fd(), (cbsize + bufsize) as i64) != 0 {
            return Err(errno_with_hint("ftruncate"));
        }
        let map = doublemap(f.as_raw_fd(), cbsize, bufsize)?;

        // initialize control block
        let cbp = map.ptr() as *mut ControlBlock;
        cbp.write(ControlBlock::default());

        // default initialize elems.
        // this is required to make sure writer always sees initialized elements
        let buffer = map.ptr().add(cbsize).cast::<T>();
        let initmap = MemoryMapInitialized::new(map, buffer, capacity);

        (initmap, buffer)
    };
    let shared_map = std::sync::Arc::new(initmap);

    Ok((
        Writer::new(shared_map.clone(), buffer, capacity),
        Reader::new(shared_map, buffer, capacity),
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cueue<T>(requested_capacity: usize) -> Result<(Writer<T>, Reader<T>), std::io::Error>
where
    T: Default,
{
    todo!("Only Linux and macOS are supported so far");
}

#[cfg(test)]
mod tests;
