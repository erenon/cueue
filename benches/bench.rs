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
            let _rr = r.begin_read();
            r.end_read();
        }
    });

    b.iter(move || {
        while w.begin_write_if_needed(16) == false {}
        unsafe {
            w.unchecked_write(b"123456789abcdefh");
        }
        w.end_write();
    });

    run.store(false, Ordering::Release);
    rt.join().unwrap();
}
