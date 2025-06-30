//! Example of inter-process communication
//!
//! Start this example first, then `ipc_read`.

use std::ffi::CString;
use std::io::{Read, Write};

#[cfg(target_os = "linux")]
type Mode = libc::mode_t;

#[cfg(target_os = "macos")]
type Mode = libc::c_uint;

fn main() -> std::io::Result<()> {
    let path = CString::new("cueue_ipc").unwrap();
    let mode = (libc::S_IRUSR | libc::S_IWUSR) as Mode;
    let f = unsafe { libc::shm_open(path.as_ptr(), libc::O_RDWR | libc::O_CREAT, mode) };
    if f < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let (mut w, _) = cueue::cueue_in_fd(f, Some(1 << 20))?;

    loop {
        print!("> ");
        std::io::stdout().flush()?;
        let buf = w.write_chunk();
        match std::io::stdin().read(buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                w.commit(n);
            }
        }
    }

    unsafe {
        libc::shm_unlink(path.as_ptr());
    }

    Ok(())
}
