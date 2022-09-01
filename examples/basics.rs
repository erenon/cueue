// An example of constructing, writing and reading a cueue

fn main() {
    // Create a cueue with capacity at least 1M.
    // (The actual capacity will be rounded up to match system requirements, if needed)
    // w and r are the write and read handles of the cueue, respectively.
    // These handles can be sent between threads, but cannot be duplicated.
    let (mut w, mut r) = cueue::cueue(1 << 20).unwrap();

    // To write a cueue, first we need to make sure there's enough space for writing.
    // w.begin_write() will make sure the maximum number of bytes available at that time
    // for writing will be reserved.
    // w.begin_write_if_needed(size) is a convenience wrapper, that calls begin_write
    // only if there's less than size bytes available.

    // Check if there are 9 bytes free for writing in the cueue.
    if w.begin_write_if_needed(3 + 3 + 3) {
        // If yes, write whatever we want
        println!("Write foo");
        w.write(b"foo");
        println!("Write bar");
        w.write(b"bar");
        println!("Write baz");
        w.write(b"baz");
        // When done, make the written are available for reading.
        // Without this, the reader will not see the written but not committed changes.
        w.end_write();
    }

    // Now read whatever is in the queue
    let read_result = r.begin_read();
    assert_eq!(read_result, b"foobarbaz");
    println!("Read {}", String::from_utf8_lossy(read_result));
    // Mark the previously returned slice consumed, making it available for writing.
    r.end_read();
}
