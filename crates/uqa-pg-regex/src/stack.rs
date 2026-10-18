//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stack headroom for the engine's recursion.
//!
//! `PostgreSQL`'s engine recurses on the C stack and checks its depth where it recurses: `stack_is_too_deep` compares the stack in use with `max_stack_depth`, which `PostgreSQL` keeps at least `STACK_DEPTH_SLOP` below the kernel's stack limit so that the frames beyond the last check always fit. A thread here may have less stack than that, so every compilation and execution runs where [`MAX_STACK_DEPTH`] plus [`STACK_DEPTH_SLOP`] are available: on the current stack when it has that much, and otherwise on a separately allocated stack of the same thread.
//!
//! An operation whose stack lacks that headroom but still has a useful amount runs there first, limited to the depth that leaves [`STACK_DEPTH_SLOP`] free. If it needs more, the engine abandons it at that depth check as it abandons a canceled operation, and the operation runs again from the start on a separate stack. Ordinary operations on threads with a few megabytes of stack therefore never allocate one, and no result depends on the thread's stack.
//!
//! Under Emscripten the engine's calls run on the WebAssembly call stack, which can be neither measured nor switched. There the C shim maintains a conservative upper bound on the engine's live frames, across every operation on the thread, and each frame is charged a fixed depth calibrated so that [`MAX_STACK_DEPTH`] corresponds to half of V8's default stack at the largest engine frame measured. Every compilation and execution therefore fails with `REG_ETOOBIG` before it can exhaust the call stack, whatever its pattern and input.

/// `PostgreSQL`'s default `max_stack_depth` of 2 MB: the deepest engine stack, measured from the start of an operation, that an operation may use.
///
/// The engine checks its depth where it recurses. Beyond this depth an operation fails with `REG_ETOOBIG` ("regular expression is too complex"), as `PostgreSQL`'s does at its default limit, without consulting [`crate::RegexResources::stack_too_deep`], which may impose a lower limit. Every operation runs with stack for this depth and [`STACK_DEPTH_SLOP`] beyond it, so any limit up to it is safe on every thread.
pub const MAX_STACK_DEPTH: usize = 2 * 1024 * 1024;

/// `PostgreSQL`'s `STACK_DEPTH_SLOP`: stack kept free beyond the deepest point the engine has checked, for the frames it does not check, the wrapper's callbacks and the resource hooks they call.
pub const STACK_DEPTH_SLOP: usize = 512 * 1024;

/// Stack the wrapper uses outside the engine's measurement: between measuring the remaining stack and entering the engine, and for switching to a separate stack.
#[cfg(not(target_family = "wasm"))]
const ENTRY_RESERVE: usize = 64 * 1024;

/// The least depth worth attempting on the current stack; ordinary operations need well under it.
#[cfg(not(target_family = "wasm"))]
const MIN_ATTEMPT_DEPTH: usize = 64 * 1024;

/// A separately allocated stack: the full depth and slop, the wrapper's entry and the switch itself.
#[cfg(not(target_family = "wasm"))]
const SEGMENT_SIZE: usize = MAX_STACK_DEPTH + STACK_DEPTH_SLOP + 2 * ENTRY_RESERVE;

/// The outcome of one attempt at an engine operation.
pub(crate) enum Attempt<T> {
    /// The operation finished, successfully or with an error.
    Finished(T),
    /// The engine needed more stack than the attempt's depth limit allowed.
    Restart,
}

impl<T> Attempt<T> {
    pub(crate) fn map<U>(self, finish: impl FnOnce(T) -> U) -> Attempt<U> {
        match self {
            Self::Finished(outcome) => Attempt::Finished(finish(outcome)),
            Self::Restart => Attempt::Restart,
        }
    }
}

/// Run one engine operation with stack headroom. `attempt` receives the depth its engine stack may reach, at most [`MAX_STACK_DEPTH`], and must leave nothing behind when it asks for a restart.
///
/// Returns `None` when the operation needs more stack than the thread can provide, which happens only on a target whose stack `stacker` cannot switch; the engine reports that as `REG_ETOOBIG`, as `PostgreSQL` does at its stack limit. A failed stack allocation returns `OutOfMemory`; an operation panic propagates only after the caller has released its compilation resources.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn run<T>(
    mut attempt: impl FnMut(usize) -> Attempt<T>,
) -> Result<Option<T>, crate::RegexError> {
    let depth = usable_depth();
    // An attempt allowed the full depth is never restarted: the engine fails beyond MAX_STACK_DEPTH instead.
    if depth >= MIN_ATTEMPT_DEPTH {
        if let Attempt::Finished(outcome) = attempt(depth) {
            return Ok(Some(outcome));
        }
    }
    grow(|| match attempt(usable_depth()) {
        Attempt::Finished(outcome) => Some(outcome),
        Attempt::Restart => None,
    })
}

/// Stacker reports stack allocation failures by panicking before calling the operation. Preserve an operation's own panic, but expose allocation failure through the resource error contract.
#[cfg(not(target_family = "wasm"))]
fn grow<T>(operation: impl FnOnce() -> T) -> Result<T, crate::RegexError> {
    let mut entered = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(test)]
        assert!(
            !FAIL_ALLOCATION.with(|failure| failure.replace(false)),
            "injected stack allocation failure"
        );
        stacker::grow(SEGMENT_SIZE, || {
            entered = true;
            operation()
        })
    }));
    match result {
        Ok(value) => Ok(value),
        Err(payload) if entered => std::panic::resume_unwind(payload),
        Err(_) => Err(crate::RegexError::OutOfMemory),
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
std::thread_local! {
    static FAIL_ALLOCATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, not(target_family = "wasm")))]
pub(crate) fn fail_next_allocation() {
    FAIL_ALLOCATION.with(|failure| failure.set(true));
}

/// WebAssembly runs the engine's calls on its own call stack, which cannot be switched, so the operation runs where it is; under Emscripten the depth checks count the engine's live frames, see [`depth`].
#[cfg(target_family = "wasm")]
pub(crate) fn run<T>(
    mut attempt: impl FnMut(usize) -> Attempt<T>,
) -> Result<Option<T>, crate::RegexError> {
    Ok(match attempt(MAX_STACK_DEPTH) {
        Attempt::Finished(outcome) => Some(outcome),
        Attempt::Restart => None,
    })
}

/// The engine stack depth of a measure that the C shim reports at a depth check: the bytes of stack used since the operation entered the engine.
#[cfg(not(target_os = "emscripten"))]
pub(crate) fn depth(measure: usize) -> usize {
    measure
}

/// Under Emscripten the C shim reports the conservative upper bound on live engine frames of the thread, and each frame is charged [`WASM_FRAME_BYTES`], so that [`WASM_FRAME_BUDGET`] frames reach [`MAX_STACK_DEPTH`].
#[cfg(target_os = "emscripten")]
pub(crate) fn depth(measure: usize) -> usize {
    measure.saturating_mul(WASM_FRAME_BYTES)
}

/// V8's default stack in Node.js 22, 864 KB, the smaller of Node.js's default and Chrome's 984 KB.
#[cfg(target_os = "emscripten")]
const V8_DEFAULT_STACK: usize = 864 * 1024;

/// The most V8 stack that one engine frame took in the calibration at [`WASM_FRAME_BUDGET`], rounded up.
#[cfg(target_os = "emscripten")]
const WASM_FRAME_STACK: usize = 296;

/// The engine frames that operations on one thread may keep live under Emscripten: as many as fit in half of V8's default stack at the largest frame measured, so that the embedding keeps the other half for its own frames. With the values above this is 1494 frames, about 432 KiB of V8 stack.
///
/// Calibration on a release build for `wasm32-unknown-emscripten` (Rust 1.90.0, Emscripten 6.0.3, `-O2`) in Node.js 22.12 with V8's default stack: each shape ran with a hook that never refuses and a frame charge of one byte, so that the depth reported the frame count, which the hook printed every 16 frames until the stack was exhausted with a `RangeError`. Bytes per frame are the default stack divided by the frames last printed, an upper bound because up to 15 more frames were live and the embedding's own frames are counted as engine frames. The baseline tier ran with `--liftoff --no-wasm-tier-up` and the optimizing tier with `--no-liftoff`.
///
/// | Operation and shape | Baseline frames | Bytes per frame | Optimizing frames | Bytes per frame |
/// |---|---|---|---|---|
/// | compile, 5000 nested capturing groups | 3987 | 221.9 | 3587 | 246.7 |
/// | compile, 5000 nested non-capturing groups | 3987 | 221.9 | 3587 | 246.7 |
/// | compile, 50000-character literal | 6723 | 131.6 | 5987 | 147.8 |
/// | compile, `(a{255}){200}` | 5987 | 147.8 | 5987 | 147.8 |
/// | compile, 10000 nested lookaheads | 3987 | 221.9 | 3587 | 246.7 |
/// | compile, 10000 capturing groups and a back-reference | 2995 | 295.4 | 2995 | 295.4 |
/// | match, 680 nested capturing groups | 324 | 193.8 | 276 | 227.5 |
/// | match, 1100 capturing groups and a back-reference | 324 | 193.8 | 276 | 227.5 |
/// | match, 1600 nested lookaheads | 213 | 294.8 | 213 | 294.8 |
///
/// Each pattern's compilation exhausts the stack before its match can, so the matches ran after a recursion with a fixed frame had used the stack down to 640 of the 9020 levels it reaches, about 62.8 KB at 98.1 bytes per level, and their bytes per frame divide that remainder by the frames reached. The largest value, 295.4 bytes, rounds up to [`WASM_FRAME_STACK`].
#[cfg(target_os = "emscripten")]
const WASM_FRAME_BUDGET: usize = V8_DEFAULT_STACK / 2 / WASM_FRAME_STACK;

/// The depth charged per live engine frame under Emscripten, so that [`WASM_FRAME_BUDGET`] frames reach [`MAX_STACK_DEPTH`]; the depth check then admits at most 1493 frames.
#[cfg(target_os = "emscripten")]
const WASM_FRAME_BYTES: usize = MAX_STACK_DEPTH.div_ceil(WASM_FRAME_BUDGET);

// The frame counts stated above follow from the calibration.
#[cfg(target_os = "emscripten")]
const _: () = assert!(WASM_FRAME_BUDGET == 1494 && MAX_STACK_DEPTH / WASM_FRAME_BYTES == 1493);

/// The depth an attempt on the current stack may reach while leaving the slop and the wrapper's entry free, at most [`MAX_STACK_DEPTH`]; zero when the remaining stack is unknown.
#[cfg(not(target_family = "wasm"))]
fn usable_depth() -> usize {
    stacker::remaining_stack()
        .map_or(0, |remaining| {
            remaining.saturating_sub(STACK_DEPTH_SLOP + ENTRY_RESERVE)
        })
        .min(MAX_STACK_DEPTH)
}
