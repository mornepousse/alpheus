//! The notification server never reads an image (notifications spec § 2): a Notify carrying an
//! 8 MiB `image-data` decodes with no more allocation than one without it. Measured by this test
//! binary's own counting allocator, per thread, so nothing else in the binary blurs the count.

use alpheus_demon::session_bus::codec::{Inbound, inbound};
use alpheus_demon::session_bus::{NOTIFICATIONS_INTERFACE, NOTIFICATIONS_PATH};
use dbus::Message;
use dbus::arg::{IterAppend, Variant};
use dbus::strings::Signature;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every call goes to the system allocator unchanged; counting is a side effect on a
// const-initialised thread-local, which never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATED.try_with(|a| a.set(a.get() + layout.size()));
        // SAFETY: the caller's contract, passed on.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATED.try_with(|a| a.set(a.get() + new_size));
        // SAFETY: the caller's contract, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A Notify with an `image-data` of `image` bytes (none: no such hint), then an urgency hint.
fn notify(image: Option<usize>) -> Message {
    let sig = |s: &str| Signature::new(s).unwrap();
    let mut m = Message::new_method_call(":1.1", NOTIFICATIONS_PATH, NOTIFICATIONS_INTERFACE, "Notify").unwrap();
    m.set_serial(9);
    let mut i = IterAppend::new(&mut m);
    i.append("Firefox");
    i.append(0u32);
    i.append("firefox");
    i.append("Screenshot saved");
    i.append("~/Pictures/shot.png");
    i.append(Vec::<&str>::new());
    i.append_dict(&sig("s"), &sig("v"), |d| {
        if let Some(bytes) = image {
            d.append_dict_entry(|e| {
                e.append("image-data");
                e.append_variant(&sig("(iiibiiay)"), |v| {
                    v.append_struct(|s| {
                        s.append(2048i32);
                        s.append(1024i32);
                        s.append(8192i32);
                        s.append(true);
                        s.append(8i32);
                        s.append(4i32);
                        s.append(vec![7u8; bytes]);
                    })
                })
            });
        }
        d.append_dict_entry(|e| {
            e.append("urgency");
            e.append(Variant(2u8));
        });
    });
    i.append(-1i32);
    m
}

/// What `inbound(m)` allocates on this thread, and its answer.
fn measured(m: &Message) -> (Inbound, usize) {
    let before = ALLOCATED.with(Cell::get);
    let decoded = inbound(m);
    (decoded, ALLOCATED.with(Cell::get) - before)
}

#[test]
fn an_8_mib_image_is_never_decoded() {
    let (plain, big) = (notify(None), notify(Some(8 << 20)));
    let (a, without) = measured(&plain);
    let (b, with) = measured(&big);
    assert!(matches!(a, Inbound::Notify(_)), "{a:?}");
    assert_eq!(a, b, "the same notification either way");
    assert!(with < 64 * 1024, "{with} bytes allocated: the image was read");
    assert!(with <= without + 4096, "{without} bytes without the image, {with} with it");
}
