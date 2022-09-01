# Cueue

A high performance, single-producer, single-consumer, bounded circular buffer
of contiguous bytes, that supports lock-free atomic batch operations,
suitable for inter-thread communication.

## Example

```rust
fn main() {
    let (mut w, mut r) = cueue::cueue(1 << 20).unwrap();

    w.begin_write();
    assert!(w.write_capacity() >= 9);
    w.write(b"foo");
    w.write(b"bar");
    w.write(b"baz");
    w.end_write();

    let read_result = r.begin_read();
    assert_eq!(read_result, b"foobarbaz");
    r.end_read();
}
```

A bounded `cueue` of requested capacity is referenced by a single Writer and a single Reader.
The Writer can request space to write (`begin_write`, `begin_write_if_needed`),
limited by the queue capacity minus the already committed but unread space.
Requested space can written to (`write`, possibly multiple times), then committed (`end_write`).

The Reader can check out the written bytes (`begin_read`), process it at will,
then mark it as consumed (`end_read`). The returned slice of bytes might be a result
of multiple writer commits (i.e: the reading is batched), but it never includes uncommitted bytes
(i.e: write commits are atomic). This prevents the reader observing partial messages.

## Use-case

This data structure is designed to allow one thread (actor) sending variable-sized messages (bytes)
to a different thread (actor), that processes the messages in batches (e.g: writes them to a file,
sends them over the network, etc.). For example, asynchronous logging.

Alternative options:

 - Use a standard channel of Strings (or `Vec<u8>`). This is slow, because strings require memory allocations,
   and with one thread allocating and the other deallocating, quickly yields to contention on the heap lock.

 - Use a standard channel of fixed size arrays. Works, but bounds the size of the messages and
   wastes memory.

 - Use two ringbuffers of `Vec<u8>` containers (one for sending data, one for reusing the consumed vectors).
   Does not allow efficient reading (separate messages are not contiguous).
   Requires to estimate the max number of messages in flight, instead of the max sum of size of messages.

This data structure uses a single byte array of the user specified capacity.
At any given time, this array is sliced into three logical parts: allocated for writing,
ready for reading, unwritten. (Any maximum two of the three can be zero sized)

`begin_write` joins the unwritten part to the part already allocated for writing:
the result is limited by the capacity minus the space ready for reading.
`end_write` makes the written space ready for reading, zeroing the slice allocated for writing.
`begin_read` determines the boundary of the space ready for reading,
`end_read` marks this space unwritten. Thanks for the truly circular nature of `cueue`,
the writer and reader can freely chase each other around.

## How Does it Work

The `cueue` constructor creates a memory area, and maps it into virtual memory twice,
the two maps next to each other. This means that for the resulting `map` of capacity `cap`,
`map[0]` and `map[cap]`, refers to the same byte. (In general, `map[N]` and `map[cap+N]` are the same
for every `0 <= N < cap` indices)

With this double map, there's no need to wrap around, this maximises the useful capacity of the queue
during any point of usage, and simplifies the indexing logic of the code. Synchronization
between writer and reader is done by atomic operations, there are no mutexes or lock ASM instruction prefixes
(on the tested platforms: x86 and M1).

(Not shown here, but this structure also allows inter-process communication using shared memory,
and data recovery from coredumps)

## Limitations

 - Supported platforms: Linux (3.17) and macOS
 - rust 1.63
 - Uses `unsafe` operations. Incorrect usage yields to crashing.

## Build and Test

```shell
$ cargo build
$ cargo test
$ cargo run --example basics
$ cargo fmt
$ cargo clippy
$ cargo bench
$ cargo doc --open
```

## Acknowledgments

This is a rust port of the [binlog][] C++ [cueue][cpp-cueue].

[binlog]: https://github.com/erenon/binlog
[cpp-cueue]: https://github.com/erenon/binlog/blob/hiperf-macos/include/binlog/detail/Cueue.hpp
