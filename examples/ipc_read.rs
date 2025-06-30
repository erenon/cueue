//! Example of inter-process communication
//!
//! Start `ipc_write` first, then this example second, while the writer is still running.

use std::ffi::CString;
use std::io::Write;

fn main() -> std::io::Result<()> {
    let path = CString::new("cueue_ipc").unwrap();
    let f = unsafe { libc::shm_open(path.as_ptr(), libc::O_RDWR, 0) };
    if f < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let (_, mut r) = cueue::cueue_in_fd(f, None)?;

    let mut out = std::io::stdout().lock();
    loop {
        let buf = r.read_chunk();
        if !buf.is_empty() {
            out.write_all(buf)?;
            r.commit();
        } else {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
