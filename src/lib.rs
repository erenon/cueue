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

mod double_map;

use double_map::MemoryMapInitialized;

use std::io::{Error, ErrorKind};
use std::sync::atomic::Ordering;

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
        let cb = mem.control_block() as *mut ControlBlock;
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
    pub const fn capacity(&self) -> usize {
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
        let cb = mem.control_block() as *mut ControlBlock;
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
    pub const fn capacity(&self) -> usize {
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

    /// Mark the slice previously acquired by `read_chunk` as consumed,
    /// making it available for writing.
    pub fn commit(&mut self) {
        let r = self.read_pos().load(Ordering::Relaxed);
        let rs = self.read_size;
        self.read_pos().store(r + rs, Ordering::Release);
    }

    /// Mark the first n elements previously acquired by `read_chunk` as consumed,
    /// making it available for writing.
    pub fn commit_read(&mut self, n: usize) {
        let rs = n as u64;
        assert!(rs <= self.read_size);
        let r = self.read_pos().load(Ordering::Relaxed);
        self.read_pos().store(r + rs, Ordering::Release);
    }

    /// Returns true, if the Writer counterpart was dropped.
    pub fn is_abandoned(&self) -> bool {
        std::sync::Arc::strong_count(&self.mem) < 2
    }

    /// Read and commit a single element, or return None if the queue was empty.
    pub fn pop(&mut self) -> Option<T> {
        let chunk = self.read_chunk();
        if chunk.is_empty() {
            None
        } else {
            let r: T = std::mem::take(unsafe { &mut *(chunk.as_ptr() as *mut T) });
            self.commit_read(1);
            Some(r)
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
pub fn cueue<T>(requested_capacity: usize) -> Result<(Writer<T>, Reader<T>), Error>
where
    T: Default,
{
    let pagesize = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };

    if std::mem::size_of::<ControlBlock>() > pagesize {
        return Err(Error::new(
            ErrorKind::Other,
            "ControlBlock does not fit in a single page",
        ));
    }

    let init_map = double_map::MemoryMapInitialized::new(requested_capacity)?;
    let buffer = init_map.buffer();
    let capacity = init_map.capacity();

    let shared_map = std::sync::Arc::new(init_map);

    Ok((
        Writer::new(shared_map.clone(), buffer, capacity),
        Reader::new(shared_map, buffer, capacity),
    ))
}
