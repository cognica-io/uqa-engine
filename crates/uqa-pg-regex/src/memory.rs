//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`-style memory contexts for the engine's allocations.
//!
//! Every engine allocation is a chunk with a header that links it into the context of the operation that allocated it. A chunk's full size, header included, is reserved through the caller's [`RegexResources`] before the allocator runs and released after the chunk is freed. When an operation is abandoned, [`MemoryContext::free_all`] frees whatever the engine still holds there, as `PostgreSQL` resets the memory context of a failed query. Hooks run only before memory is allocated or after it is freed, so a panicking hook never leaves memory behind.
#![allow(unsafe_code)] // Chunk headers and intrusive links are managed through raw pointers owned by this module.

use std::alloc::{self, Layout};
use std::ptr::{self, NonNull};

use crate::resources::RegexResources;

/// Payload alignment of every chunk, at least `max_align_t` on supported targets.
const ALIGNMENT: usize = 16;

/// `PostgreSQL`'s `MaxAllocSize`: `palloc` rejects larger requests unless they are huge allocations, which the regular-expression functions never request. Callers that convert text to code points for the engine apply the same limit to their buffers, as `regexp.c` does.
pub const MAX_ALLOC_SIZE: usize = 0x3fff_ffff;

#[repr(C)]
struct ChunkHeader {
    previous: *mut ChunkHeader,
    next: *mut ChunkHeader,
    context: *mut MemoryContext,
    total: usize,
}

/// Header size rounded up so that payloads keep [`ALIGNMENT`].
const HEADER: usize = (size_of::<ChunkHeader>() + ALIGNMENT - 1) & !(ALIGNMENT - 1);

// Blocks, headers and payloads keep ALIGNMENT, which covers every type stored in them.
const _: () = assert!(
    ALIGNMENT >= align_of::<ChunkHeader>()
        && ALIGNMENT >= align_of::<u32>()
        && HEADER.is_multiple_of(ALIGNMENT)
);

/// The header at the start of a block allocated with [`ALIGNMENT`].
#[allow(clippy::cast_ptr_alignment)] // Every block is allocated with ALIGNMENT, which covers the header's alignment.
fn block_header(block: *mut u8) -> *mut ChunkHeader {
    block.cast::<ChunkHeader>()
}

/// The header in front of a payload.
///
/// # Safety
/// `payload` must come from [`MemoryContext::allocate`] or [`MemoryContext::reallocate`] and still be live.
unsafe fn payload_header(payload: NonNull<u8>) -> *mut ChunkHeader {
    block_header(payload.as_ptr().sub(HEADER))
}

/// A chunk payload viewed as `u32` code points.
#[allow(clippy::cast_ptr_alignment)] // Payloads keep ALIGNMENT, which covers the alignment of u32.
pub(crate) fn code_point_slots(payload: NonNull<u8>) -> *mut u32 {
    payload.as_ptr().cast::<u32>()
}

/// The live chunks of one context, linked in a circle through a sentinel. A context must not move after [`MemoryContext::initialize`].
pub(crate) struct MemoryContext {
    sentinel: ChunkHeader,
    bytes: usize,
}

impl MemoryContext {
    /// A context that must be initialized in place before use.
    pub(crate) const fn unlinked() -> Self {
        Self {
            sentinel: ChunkHeader {
                previous: ptr::null_mut(),
                next: ptr::null_mut(),
                context: ptr::null_mut(),
                total: 0,
            },
            bytes: 0,
        }
    }

    /// Link the empty circle at the context's final address.
    ///
    /// # Safety
    /// `this` must be valid for writes and must not move while chunks are allocated in it.
    pub(crate) unsafe fn initialize(this: *mut Self) {
        let sentinel = &raw mut (*this).sentinel;
        (*sentinel).previous = sentinel;
        (*sentinel).next = sentinel;
        (*sentinel).context = this;
        (*this).bytes = 0;
    }

    /// Bytes held by live chunks, headers included.
    ///
    /// # Safety
    /// `this` must be an initialized context.
    pub(crate) unsafe fn bytes(this: *const Self) -> usize {
        (*this).bytes
    }

    /// Reserve and allocate a chunk with `size` payload bytes. `None` means the reservation was refused or the allocator failed; nothing remains reserved then.
    ///
    /// # Safety
    /// `this` must be an initialized context.
    pub(crate) unsafe fn allocate(
        this: *mut Self,
        resources: &mut dyn RegexResources,
        size: usize,
    ) -> Option<NonNull<u8>> {
        let total = HEADER.checked_add(size)?;
        let layout = Layout::from_size_align(total, ALIGNMENT).ok()?;
        if !resources.reserve(total) {
            return None;
        }
        let block = block_header(alloc::alloc(layout));
        if block.is_null() {
            resources.release(total);
            return None;
        }
        let sentinel = &raw mut (*this).sentinel;
        let last = (*sentinel).previous;
        block.write(ChunkHeader {
            previous: last,
            next: sentinel,
            context: this,
            total,
        });
        (*last).next = block;
        (*sentinel).previous = block;
        (*this).bytes += total;
        Some(NonNull::new_unchecked(block.cast::<u8>().add(HEADER)))
    }

    /// Resize a chunk, reserving growth before and releasing shrinkage after the allocator runs. `None` leaves the original chunk and its reservation unchanged.
    ///
    /// # Safety
    /// `payload` must come from [`MemoryContext::allocate`] or this function and still be live.
    pub(crate) unsafe fn reallocate(
        payload: NonNull<u8>,
        resources: &mut dyn RegexResources,
        size: usize,
    ) -> Option<NonNull<u8>> {
        let header = payload_header(payload);
        let old_total = (*header).total;
        let total = HEADER.checked_add(size)?;
        Layout::from_size_align(total, ALIGNMENT).ok()?;
        if total > old_total && !resources.reserve(total - old_total) {
            return None;
        }
        let previous = (*header).previous;
        let next = (*header).next;
        let context = (*header).context;
        let old_layout = Layout::from_size_align_unchecked(old_total, ALIGNMENT);
        let block = block_header(alloc::realloc(header.cast::<u8>(), old_layout, total));
        if block.is_null() {
            if total > old_total {
                resources.release(total - old_total);
            }
            return None;
        }
        (*block).total = total;
        (*previous).next = block;
        (*next).previous = block;
        (*context).bytes = (*context).bytes - old_total + total;
        if total < old_total {
            resources.release(old_total - total);
        }
        Some(NonNull::new_unchecked(block.cast::<u8>().add(HEADER)))
    }

    /// Unlink and free a chunk, then release its reservation.
    ///
    /// # Safety
    /// `payload` must come from [`MemoryContext::allocate`] or [`MemoryContext::reallocate`] and still be live.
    pub(crate) unsafe fn free(payload: NonNull<u8>, resources: &mut dyn RegexResources) {
        let header = payload_header(payload);
        let total = (*header).total;
        let context = (*header).context;
        (*(*header).previous).next = (*header).next;
        (*(*header).next).previous = (*header).previous;
        (*context).bytes -= total;
        alloc::dealloc(
            header.cast::<u8>(),
            Layout::from_size_align_unchecked(total, ALIGNMENT),
        );
        resources.release(total);
    }

    /// Free every chunk still linked into the context and return their bytes, which the caller releases after all memory is freed.
    ///
    /// # Safety
    /// `this` must be an initialized context, and no live pointer into its chunks may be used afterwards.
    pub(crate) unsafe fn free_all(this: *mut Self) -> usize {
        let sentinel = &raw mut (*this).sentinel;
        let mut chunk = (*sentinel).next;
        let mut released = 0usize;
        while chunk != sentinel {
            let next = (*chunk).next;
            let total = (*chunk).total;
            alloc::dealloc(
                chunk.cast::<u8>(),
                Layout::from_size_align_unchecked(total, ALIGNMENT),
            );
            released += total;
            chunk = next;
        }
        (*sentinel).previous = sentinel;
        (*sentinel).next = sentinel;
        (*this).bytes = 0;
        released
    }
}

/// A context on the heap, for a compiled expression that outlives the call that created it. Its own allocation is reserved like an engine chunk.
pub(crate) struct OwnedContext {
    context: NonNull<MemoryContext>,
}

impl OwnedContext {
    const LAYOUT: Layout = Layout::new::<MemoryContext>();

    #[allow(clippy::cast_ptr_alignment)] // The block is allocated with the context's own layout.
    fn context_block(block: *mut u8) -> *mut MemoryContext {
        block.cast::<MemoryContext>()
    }

    /// `None` when the reservation is refused or the allocator fails.
    pub(crate) fn new(resources: &mut dyn RegexResources) -> Option<Self> {
        if !resources.reserve(Self::LAYOUT.size()) {
            return None;
        }
        // SAFETY: the layout has a non-zero size; a successful allocation is initialized before use and never moves.
        unsafe {
            let Some(context) = NonNull::new(Self::context_block(alloc::alloc(Self::LAYOUT)))
            else {
                resources.release(Self::LAYOUT.size());
                return None;
            };
            context.as_ptr().write(MemoryContext::unlinked());
            MemoryContext::initialize(context.as_ptr());
            Some(Self { context })
        }
    }

    pub(crate) fn as_ptr(&self) -> *mut MemoryContext {
        self.context.as_ptr()
    }

    /// Bytes reserved for the context and its live chunks.
    pub(crate) fn reserved_bytes(&self) -> usize {
        // SAFETY: the context was initialized by `new` and is alive until `release`.
        Self::LAYOUT.size() + unsafe { MemoryContext::bytes(self.context.as_ptr()) }
    }

    /// Free every chunk but keep the context, as `PostgreSQL` resets a memory context, then release their reservations.
    ///
    /// # Safety
    /// No pointer into the context's chunks may be used afterwards.
    pub(crate) unsafe fn reset(&self, resources: &mut dyn RegexResources) {
        let chunks = MemoryContext::free_all(self.context.as_ptr());
        if chunks > 0 {
            resources.release(chunks);
        }
    }

    /// Free every remaining chunk and the context itself, then release their reservations. All memory is freed before the hook runs, so a panicking hook cannot leak it.
    pub(crate) fn release(self, resources: &mut dyn RegexResources) {
        // SAFETY: this owner is consumed, so no pointer into the context or its chunks remains in use.
        let chunks = unsafe {
            let chunks = MemoryContext::free_all(self.context.as_ptr());
            alloc::dealloc(self.context.as_ptr().cast::<u8>(), Self::LAYOUT);
            chunks
        };
        resources.release(chunks + Self::LAYOUT.size());
    }
}

#[cfg(test)]
mod tests;
