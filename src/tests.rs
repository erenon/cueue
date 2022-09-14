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
    let (w, r) = cueue::<'_, u8>(16).unwrap();
    assert_eq!(w.capacity(), r.capacity());
    assert!(w.capacity() >= 4096);
}

#[test]
fn test_writer() {
    let (mut w, r) = cueue::<'_, u8>(16).unwrap();

    let cap = w.capacity();

    let buf = w.write_chunk();
    assert_eq!(buf.len(), cap);
    w.commit(0);

    let buf = w.write_chunk();
    assert_eq!(buf.len(), cap);
    w.commit(3);

    let buf = w.write_chunk();
    assert_eq!(buf.len(), cap - 3);

    assert!(!w.is_abandoned());
    std::mem::drop(r);
    assert!(w.is_abandoned());
}

#[test]
fn test_reader() {
    let (mut w, mut r) = cueue(16).unwrap();

    let empty = r.read_chunk();
    assert_eq!(empty.len(), 0);
    r.commit();

    let buf = w.write_chunk();
    buf[..3].copy_from_slice(b"foo");
    w.commit(3);

    let foo = r.read_chunk();
    assert_eq!(foo, b"foo");
    r.commit();

    assert!(!r.is_abandoned());
    std::mem::drop(w);
    assert!(r.is_abandoned());
}

#[test]
fn test_full() {
    let (mut w, mut r) = cueue::<'_, u8>(16).unwrap();

    let buf = w.write_chunk();
    let buflen = buf.len();
    assert_eq!(buf.len(), w.capacity());
    w.commit(buflen);

    let empty = w.write_chunk();
    assert_eq!(empty.len(), 0);

    let full = r.read_chunk();
    assert_eq!(full.len(), buflen);
    assert_eq!(full.len(), r.capacity());
}

#[test]
fn test_reuse() {
    let (mut w, mut r) = cueue(16).unwrap();

    // fill the queue with strings
    let buf = w.write_chunk();
    for s in buf.into_iter() {
        *s = "foobar";
    }
    let buflen = buf.len();
    w.commit(buflen);

    // consume everything
    let full = r.read_chunk();
    assert_eq!(full.len(), buflen);
    r.commit();

    // try writing again
    let buf = w.write_chunk();
    assert_eq!(buf[0], "foobar");
}

#[test]
fn test_cueue_threaded_w_r() {
    let (mut w, mut r) = cueue(16).unwrap();
    let maxi = 1_000_000;

    let wt = std::thread::spawn(move || {
        let mut msg: u8 = 0;
        for _ in 0..maxi {
            let buf = loop {
                let buf = w.write_chunk();
                if buf.len() > 0 {
                    break buf;
                }
            };
            buf[0] = msg;
            w.commit(1);

            msg = msg.wrapping_add(1);
        }
    });

    let rt = std::thread::spawn(move || {
        let mut emsg: u8 = 0;
        let mut i = 0;
        while i < maxi {
            let rr = r.read_chunk();
            for msg in rr {
                assert_eq!(*msg, emsg);
                emsg = emsg.wrapping_add(1);
                i += 1;
            }
            r.commit();
        }
    });

    wt.join().unwrap();
    rt.join().unwrap();
}
