use crate::*;

#[test]
fn test_next_power_two() {
    assert_eq!(1, next_power_two(0).unwrap());
    assert_eq!(1, next_power_two(1).unwrap());
    assert_eq!(2, next_power_two(2).unwrap());
    assert_eq!(4, next_power_two(3).unwrap());
    assert_eq!(4, next_power_two(4).unwrap());
    assert_eq!(8, next_power_two(5).unwrap());
    assert_eq!(4096, next_power_two(4095).unwrap());
    assert_eq!(4096, next_power_two(4096).unwrap());

    assert_eq!(1 << 63, next_power_two(1 << 63).unwrap());
    assert!(next_power_two((1 << 63) + 1).is_err());
}

#[test]
fn test_capacity() {
    let (w, r) = cueue(16).unwrap();
    assert_eq!(w.capacity(), r.capacity());
    assert!(w.capacity() >= 4096);
}

#[test]
fn test_writer() {
    let (mut w, _) = cueue(16).unwrap();

    let cap = w.capacity();

    assert_eq!(w.write_capacity(), 0);

    assert_eq!(w.begin_write(), cap);
    assert_eq!(w.write_capacity(), cap);
    w.end_write();

    assert_eq!(w.write_capacity(), cap);
    w.write(b"foo");
    assert_eq!(w.write_capacity(), cap - 3);
}

#[test]
fn test_writer_on_demand() {
    let (mut w, _) = cueue(16).unwrap();
    let cap = w.capacity();

    assert_eq!(w.begin_write_if_needed(1), true);
    assert_eq!(w.begin_write_if_needed(2), true);
    assert_eq!(w.begin_write_if_needed(cap), true);
    assert_eq!(w.begin_write_if_needed(cap + 1), false);
}

#[test]
fn test_reader() {
    let (mut w, mut r) = cueue(16).unwrap();

    let empty = r.begin_read();
    assert_eq!(empty.len(), 0);
    r.end_read();

    w.begin_write();
    w.write(b"foo");
    w.end_write();

    let foo = r.begin_read();
    assert_eq!(foo, b"foo");
    r.end_read();
}

#[test]
fn test_full() {
    let (mut w, mut r) = cueue(16).unwrap();

    let cap = w.begin_write();
    for _ in 0..cap {
        w.write(b"x");
    }
    w.end_write();
    assert_eq!(w.write_capacity(), 0);

    let full = r.begin_read();
    assert_eq!(full.len(), cap);
}

#[test]
fn test_cueue_threaded_w_r() {
    let (mut w, mut r) = cueue(16).unwrap();
    let maxi = 1_000_000;

    let wt = std::thread::spawn(move || {
        let mut msg: u8 = 0;
        for _ in 0..maxi {
            while w.begin_write_if_needed(1) == false {}
            w.write(&[msg; 1]);
            w.end_write();

            msg = msg.wrapping_add(1);
        }
    });

    let rt = std::thread::spawn(move || {
        let mut emsg: u8 = 0;
        let mut i = 0;
        while i < maxi {
            let rr = r.begin_read();
            for msg in rr {
                assert_eq!(msg, msg);
                emsg = emsg.wrapping_add(1);
                i += 1;
            }
            r.end_read();
        }
    });

    wt.join().unwrap();
    rt.join().unwrap();
}
