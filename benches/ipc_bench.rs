//! Send timestamps from one process to the other, print send-recv latency (nanoseconds) of each message
//!
//! See ipc_bench.sh

const NAME: &str = "cueue_ipc_bench";

const N: usize = 100_000;

const MSG_SIZE: usize = 20;

const DELAY: std::time::Duration = std::time::Duration::from_micros(10);

use std::ffi::CString;

fn main() -> std::io::Result<()> {
    let runtime = DELAY * N as u32;
    if runtime >= std::time::Duration::from_secs(10) {
        eprintln!("Expected runtime: {runtime:?}");
    }

    let mode = std::env::args().nth(1).unwrap();
    if mode == "writer" {
        writer_main()
    } else {
        reader_main()
    }
}

fn writer_main() -> std::io::Result<()> {
    let name = CString::new(NAME).unwrap();
    let f = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR, 0) };
    if f < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let (mut w, _) = cueue::cueue_in_fd(f, None)?;

    for _ in 0..N {
        let buf = w.write_chunk();
        if buf.len() >= MSG_SIZE {
            let ns = monotonic_nanoseconds();
            buf[0..8].copy_from_slice(&ns.to_le_bytes());
            w.commit(MSG_SIZE);
            std::thread::sleep(DELAY);
        }
    }

    Ok(())
}

fn reader_main() -> std::io::Result<()> {
    let mut latencies = Vec::with_capacity(N);

    let name = CString::new(NAME).unwrap();
    let mode = libc::S_IRUSR | libc::S_IWUSR;
    let f = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDWR | libc::O_CREAT, mode) };
    if f < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let (_, mut r) = cueue::cueue_in_fd(f, Some(1 << 20))?;

    while latencies.len() < N {
        let buf = r.read_chunk();
        let recv = monotonic_nanoseconds();
        let mut ns = [0u8; 8];
        let mut offset = 0;
        while offset + MSG_SIZE <= buf.len() {
            ns.copy_from_slice(&buf[offset..offset + 8]);
            let sent = u64::from_le_bytes(ns);
            assert!(
                sent < recv,
                "back to the future: sent: {sent}, recv: {recv}"
            );
            latencies.push(recv - sent);
            offset += MSG_SIZE;
        }
        r.commit();
    }

    let mut out = std::io::stdout().lock();
    for l in latencies {
        use std::io::Write;
        write!(out, "{l}\n")?;
    }

    Ok(())
}

fn monotonic_nanoseconds() -> u64 {
    let ts = unsafe {
        let mut ts = std::mem::MaybeUninit::<libc::timespec>::uninit();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, ts.as_mut_ptr());
        ts.assume_init()
    };
    (ts.tv_sec * 1_000_000_000 + ts.tv_nsec) as u64

    // The bench spends 80% of its time in clock_gettime.
    // To get more accurate results, use TSC instead:
    //
    //  use core::arch::x86_64::_rdtsc;
    //  unsafe { _rdtsc() }
}
