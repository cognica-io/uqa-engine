//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Process-wide member lists of the locale-dependent character classes.
//!
//! `PostgreSQL`'s `pg_ctype_get_cache` probes every code point up to a locale's limit once per backend and keeps the result for the backend's lifetime. Here each list is built once per process under a [`OnceLock`] and then shared immutably by every thread. A list depends only on the locale token and the class, because the probe functions are fixed per class and the locale strategies are fixed per token.
#![allow(unsafe_code)] // C calls this function with a probe function pointer and output slots.

use std::ffi::c_int;
use std::sync::OnceLock;

/// `NUM_CCLASSES` in `regguts.h`.
const CLASSES: usize = 14;
/// Locale tokens 1 through 4 of `uqa_pg_regex.h`.
const LOCALES: usize = 4;

type Probe = unsafe extern "C" fn(u32) -> c_int;

struct ClassMembers {
    chrs: Box<[u32]>,
    ranges: Box<[u32]>,
}

static MEMBERS: [[OnceLock<ClassMembers>; CLASSES]; LOCALES] =
    [const { [const { OnceLock::new() }; CLASSES] }; LOCALES];

/// Scan `0..=max_chr` exactly as `pg_ctype_get_cache` does: runs of one code point become chrs, longer runs become ranges.
fn scan(probe: Probe, max_chr: u32) -> ClassMembers {
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
    ClassMembers {
        chrs: chrs.into_boxed_slice(),
        ranges: ranges.into_boxed_slice(),
    }
}

fn length(values: &[u32]) -> c_int {
    c_int::try_from(values.len()).expect("class member lists are bounded by MAX_SIMPLE_CHR")
}

fn pointer(values: &[u32]) -> *const u32 {
    if values.is_empty() {
        std::ptr::null()
    } else {
        values.as_ptr()
    }
}

/// Provide the member lists for `cclasscode` under `locale`, building them with `probe` on first use. `ranges` holds pairs; `nranges` counts pairs.
#[no_mangle]
extern "C" fn uqa_pg_regex_class_members(
    locale: u32,
    cclasscode: c_int,
    probe: Probe,
    max_chr: u32,
    chrs: *mut *const u32,
    nchrs: *mut c_int,
    ranges: *mut *const u32,
    nranges: *mut c_int,
) {
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
    let members = MEMBERS[locale_index][class_index].get_or_init(|| scan(probe, max_chr));
    // SAFETY: C passes valid output slots; the lists live in static storage and are never freed.
    unsafe {
        chrs.write(pointer(&members.chrs));
        nchrs.write(length(&members.chrs));
        ranges.write(pointer(&members.ranges));
        nranges.write(length(&members.ranges) / 2);
    }
}

/// The engine passes only validated locale tokens and its own class codes, and no longjmp may start below this Rust frame, so an invalid key stops the process.
fn abort_invalid(locale: u32, cclasscode: c_int) -> ! {
    eprintln!("uqa-pg-regex: invalid character class key (locale {locale}, class {cclasscode})");
    std::process::abort();
}
