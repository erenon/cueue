// An example of constructing, writing and reading a cueue

fn main() {
    // Create a cueue with capacity at least 1M.
    // (The actual capacity will be rounded up to match system requirements, if needed)
    // w and r are the write and read handles of the cueue, respectively.
    // These handles can be sent between threads, but cannot be duplicated.
    let (mut w, mut r) = cueue::cueue(1 << 20).unwrap();

    // To write a cueue, first we need to get a writable slice from it:
    let buf = w.write_chunk();

    // Check if there are 9 bytes free for writing in the cueue.
    if buf.len() >= 3 + 3 + 3 {
        // If yes, write whatever we want
        buf[..9].copy_from_slice(b"foobarbaz");

        // When done, make the written are available for reading.
        // Without this, the reader will not see the written but not committed changes.
        w.commit(9);
    }

    // Now read whatever is in the queue
    let read_result = r.read_chunk();
    assert_eq!(read_result, b"foobarbaz");
    println!("Read {}", String::from_utf8_lossy(read_result));
    // Mark the previously returned slice consumed, making it available for writing.
    r.commit();
}
