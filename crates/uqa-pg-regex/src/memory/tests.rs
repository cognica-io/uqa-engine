//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(Default)]
struct Ledger {
    live: usize,
    refuse: bool,
}

impl RegexResources for Ledger {
    fn reserve(&mut self, bytes: usize) -> bool {
        if self.refuse {
            return false;
        }
        self.live += bytes;
        true
    }

    fn release(&mut self, bytes: usize) {
        self.live -= bytes;
    }

    fn cancel_requested(&mut self) -> bool {
        false
    }

    fn stack_too_deep(&mut self, _depth: usize) -> bool {
        false
    }
}

#[test]
fn chunks_charge_their_headers_and_keep_their_payload_across_resizing() {
    let mut ledger = Ledger::default();
    let context = OwnedContext::new(&mut ledger).unwrap();
    let base = ledger.live;
    assert_eq!(base, size_of::<MemoryContext>());
    // SAFETY: the context is live; every pointer below is a live chunk of it.
    unsafe {
        let first = MemoryContext::allocate(context.as_ptr(), &mut ledger, 10).unwrap();
        assert_eq!(first.as_ptr() as usize % ALIGNMENT, 0);
        assert_eq!(ledger.live, base + HEADER + 10);
        ptr::copy_nonoverlapping(b"0123456789".as_ptr(), first.as_ptr(), 10);
        let second = MemoryContext::allocate(context.as_ptr(), &mut ledger, 3).unwrap();
        let grown = MemoryContext::reallocate(first, &mut ledger, 4000).unwrap();
        assert_eq!(
            std::slice::from_raw_parts(grown.as_ptr(), 10),
            b"0123456789"
        );
        assert_eq!(ledger.live, base + 2 * HEADER + 4000 + 3);
        assert_eq!(context.reserved_bytes(), ledger.live);
        let shrunk = MemoryContext::reallocate(grown, &mut ledger, 5).unwrap();
        assert_eq!(std::slice::from_raw_parts(shrunk.as_ptr(), 5), b"01234");
        assert_eq!(ledger.live, base + 2 * HEADER + 5 + 3);
        MemoryContext::free(second, &mut ledger);
        assert_eq!(ledger.live, base + HEADER + 5);
    }
    context.release(&mut ledger);
    assert_eq!(ledger.live, 0);
}

#[test]
fn refused_requests_leave_chunks_and_reservations_unchanged() {
    let mut ledger = Ledger::default();
    let context = OwnedContext::new(&mut ledger).unwrap();
    // SAFETY: the context is live; the chunk stays live until the context is released.
    unsafe {
        let chunk = MemoryContext::allocate(context.as_ptr(), &mut ledger, 8).unwrap();
        let held = ledger.live;
        ledger.refuse = true;
        assert!(MemoryContext::allocate(context.as_ptr(), &mut ledger, 8).is_none());
        assert!(MemoryContext::reallocate(chunk, &mut ledger, 64).is_none());
        assert_eq!(ledger.live, held);
        assert_eq!(context.reserved_bytes(), held);
        ledger.refuse = false;
    }
    context.release(&mut ledger);
    assert_eq!(ledger.live, 0);
}

#[test]
fn freeing_a_context_frees_every_remaining_chunk() {
    let mut ledger = Ledger::default();
    let mut context = MemoryContext::unlinked();
    // SAFETY: the context stays at this address until its chunks are freed below.
    unsafe {
        MemoryContext::initialize(&raw mut context);
        for size in [1, 100, 10_000] {
            MemoryContext::allocate(&raw mut context, &mut ledger, size).unwrap();
        }
        let freed = MemoryContext::free_all(&raw mut context);
        assert_eq!(freed, ledger.live);
        assert_eq!(MemoryContext::bytes(&raw const context), 0);
        ledger.release(freed);
    }
    assert!(OwnedContext::new(&mut Ledger {
        refuse: true,
        ..Ledger::default()
    })
    .is_none());
}
