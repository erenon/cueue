#![feature(test)]

use cueue::cueue;

extern crate test;
use self::test::Bencher;

use std::sync::atomic::Ordering;

#[bench]
fn bench_write(b: &mut Bencher) {
    let (mut w, mut r) = cueue(16).unwrap();

    let run = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let rrun = run.clone();

    let rt = std::thread::spawn(move || {
        while rrun.load(Ordering::Acquire) {
            let _rr = r.read_chunk();
            r.commit();
        }
    });

    b.iter(move || {
        let buf = loop {
            let buf = w.write_chunk();
            if buf.len() >= 16 {
                break buf;
            }
        };
        unsafe {
            std::ptr::copy_nonoverlapping(b"123456789abcdefh", buf.as_mut_ptr(), 16);
        }
        w.commit(16);
    });

    run.store(false, Ordering::Release);
    rt.join().unwrap();
}
