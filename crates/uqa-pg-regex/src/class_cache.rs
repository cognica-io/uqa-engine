//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-wide cvecs of the locale-dependent character classes.
//!
//! `PostgreSQL`'s `pg_ctype_get_cache` probes every code point up to a locale's limit once per backend and keeps the resulting cvec, outside any memory context, for the backend's lifetime. Here each cvec is built once per process under a [`OnceLock`] and then shared immutably by every thread and operation, so a class reference allocates nothing, as a hit in `PostgreSQL`'s cache does. A cvec depends only on the locale token and the class, because the probe functions are fixed per class and the locale strategies are fixed per token. The storage is never freed and is not charged to any operation's resources.
#![allow(unsafe_code)] // C calls this module with a probe function pointer and fills the cvecs it allocates.

use std::ffi::{c_int, c_void};
use std::sync::OnceLock;

/// `NUM_CCLASSES` in `regguts.h`.
const CLASSES: usize = 14;
/// Locale tokens 1 through 4 of `uqa_pg_regex.h`.
const LOCALES: usize = 4;

type Probe = unsafe extern "C" fn(u32) -> c_int;

extern "C" {
    /// `sizeof(struct cvec)`.
    fn uqa_pg_regex_cvec_size() -> usize;
    /// The alignment of `struct cvec`.
    fn uqa_pg_regex_cvec_alignment() -> usize;
    /// Write a cvec over the given arrays into `storage`.
    fn uqa_pg_regex_fill_cvec(
        storage: *mut c_void,
        chrs: *const u32,
        nchrs: c_int,
        ranges: *const u32,
        nranges: c_int,
        cclasscode: c_int,
    );
}

/// A complete cvec and the arrays it points into, which live as long as it.
struct ClassCvec {
    _chrs: Box<[u32]>,
    _ranges: Box<[u32]>,
    cvec: Box<[u64]>,
}

static CVECS: [[OnceLock<ClassCvec>; CLASSES]; LOCALES] =
    [const { [const { OnceLock::new() }; CLASSES] }; LOCALES];

/// Scan `0..=max_chr` exactly as `pg_ctype_get_cache` does: runs of one code point become chrs, longer runs become ranges.
fn scan(probe: Probe, max_chr: u32) -> (Box<[u32]>, Box<[u32]>) {
    let mut chrs = Vec::new();
    let mut ranges = Vec::new();
    let mut store = |first: u32, count: u32| {
        if count > 1 {
            ranges.extend([first, first + count - 1]);
        } else {
            chrs.push(first);
        }
    };
    let mut matches = 0u32;
    for code in 0..=max_chr {
        // SAFETY: the probe is one of the engine's pg_wc_* functions, valid for every code point.
        if unsafe { probe(code) } != 0 {
            matches += 1;
        } else if matches > 0 {
            store(code - matches, matches);
            matches = 0;
        }
    }
    if matches > 0 {
        store(max_chr + 1 - matches, matches);
    }
    (chrs.into_boxed_slice(), ranges.into_boxed_slice())
}

fn length(values: &[u32]) -> c_int {
    c_int::try_from(values.len()).expect("class member lists are bounded by MAX_SIMPLE_CHR")
}

/// `PostgreSQL` leaves an empty array NULL.
fn pointer(values: &[u32]) -> *const u32 {
    if values.is_empty() {
        std::ptr::null()
    } else {
        values.as_ptr()
    }
}

fn build(probe: Probe, max_chr: u32, cclasscode: c_int) -> ClassCvec {
    let (chrs, ranges) = scan(probe, max_chr);
    // SAFETY: both functions only report properties of the C type.
    let (size, alignment) = unsafe { (uqa_pg_regex_cvec_size(), uqa_pg_regex_cvec_alignment()) };
    assert!(
        alignment <= align_of::<u64>(),
        "struct cvec needs no more than u64 alignment"
    );
    let mut cvec = vec![0u64; size.div_ceil(size_of::<u64>())].into_boxed_slice();
    // SAFETY: the storage holds at least `size` suitably aligned bytes, and the arrays move into the same never-freed entry, so their heap buffers outlive the cvec.
    unsafe {
        uqa_pg_regex_fill_cvec(
            cvec.as_mut_ptr().cast::<c_void>(),
            pointer(&chrs),
            length(&chrs),
            pointer(&ranges),
            length(&ranges) / 2,
            cclasscode,
        );
    }
    ClassCvec {
        _chrs: chrs,
        _ranges: ranges,
        cvec,
    }
}

/// The cvec for `cclasscode` under `locale`, built with `probe` on first use. `result_cclasscode` is the class code the cvec records: `-1` when no run-time locale checks are needed.
#[no_mangle]
extern "C" fn uqa_pg_regex_class_cvec(
    locale: u32,
    cclasscode: c_int,
    result_cclasscode: c_int,
    probe: Probe,
    max_chr: u32,
) -> *mut c_void {
    let locale_index = usize::try_from(locale)
        .ok()
        .and_then(|token| token.checked_sub(1));
    let class_index = usize::try_from(cclasscode).ok();
    let (Some(locale_index), Some(class_index)) = (locale_index, class_index) else {
        abort_invalid(locale, cclasscode);
    };
    if locale_index >= LOCALES || class_index >= CLASSES {
        abort_invalid(locale, cclasscode);
    }
    let entry =
        CVECS[locale_index][class_index].get_or_init(|| build(probe, max_chr, result_cclasscode));
    // The engine only reads cached cvecs.
    entry.cvec.as_ptr().cast_mut().cast::<c_void>()
}

/// The engine passes only validated locale tokens and its own class codes, and no longjmp may start below this Rust frame, so an invalid key stops the process.
fn abort_invalid(locale: u32, cclasscode: c_int) -> ! {
    eprintln!("uqa-pg-regex: invalid character class key (locale {locale}, class {cclasscode})");
    std::process::abort();
}
