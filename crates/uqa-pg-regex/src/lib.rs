//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18.4's Advanced Regular Expression engine for UQA Engine.
//!
//! The crate compiles Henry Spencer's engine from `PostgreSQL`'s `src/backend/regex`, imported unchanged and verified against `SHA256SUMS`, and exposes it through a small safe API:
//!
//! - [`Regex::compile`] runs `pg_regcomp` on a pattern with `PostgreSQL`'s [`CompileFlags`] and a [`RegexCollation`]. [`RegexCollation::from_oid`] and [`RegexCollation::from_locale`] apply `pg_set_regex_collation`'s checks and errors before any C code runs.
//! - [`Regex::exec`] runs `pg_regexec` on code points from a search start and fills [`MatchSpan`] slots; [`Regex::subexpression_count`] is `re_nsub`.
//! - Failures are [`RegexError`] values: the engine's `REG_*` code with `pg_regerror`'s exact explanation, a cancellation, or an error `PostgreSQL` raises with its own SQLSTATE.
//! - Every engine allocation is charged to the caller's [`RegexResources`] before it happens and released after it is freed. The same hooks answer the engine's interrupt and stack-depth checks. Dropping a [`Regex`] releases every reserved byte.
//! - The engine recurses on the stack. Every operation may use [`MAX_STACK_DEPTH`], `PostgreSQL`'s default `max_stack_depth` of 2 MB, with [`STACK_DEPTH_SLOP`] to spare, on a separately allocated stack when the thread's own lacks that headroom, and fails with `REG_ETOOBIG` beyond it. Under Emscripten, whose call stack cannot be switched, the engine counts its live frames and fails with `REG_ETOOBIG` before they take half of V8's default stack.
//!
//! Character classes, case-insensitive matching and word boundaries follow the collation's strategy: `PostgreSQL`'s ASCII table for `C`, `PostgreSQL`'s own Unicode 16 tables for the builtin provider, and ICU's definitions over ICU4X Unicode 16 data for ICU collations. The C shims keep all engine state per operation and per thread, so operations on different threads are independent. `UPSTREAM.md` lists the imported files and every local adaptation.
//!
//! ```
//! use uqa_pg_regex::{MatchSpan, Regex, RegexCollation, RegexResources, REG_ADVANCED};
//!
//! struct Unlimited;
//!
//! impl RegexResources for Unlimited {
//!     fn reserve(&mut self, _bytes: usize) -> bool {
//!         true
//!     }
//!     fn release(&mut self, _bytes: usize) {}
//!     fn cancel_requested(&mut self) -> bool {
//!         false
//!     }
//!     fn stack_too_deep(&mut self, _depth: usize) -> bool {
//!         // The engine stops at MAX_STACK_DEPTH by itself; a hook may only set a lower limit.
//!         false
//!     }
//! }
//!
//! let mut resources = Unlimited;
//! let mut regex = Regex::compile("a|ab", REG_ADVANCED, RegexCollation::C, &mut resources).unwrap();
//! let input: Vec<u32> = "ab".chars().map(u32::from).collect();
//! let mut spans = [MatchSpan::UNSET; 1];
//! assert!(regex.exec(&input, 0, &mut spans).unwrap());
//! // PostgreSQL selects the longest match at the earliest position.
//! assert_eq!(spans[0].range(), Some(0..2));
//! ```

mod class_cache;
mod collation;
mod error;
mod ffi;
mod flags;
mod icu;
mod memory;
mod operation;
mod regex;
mod resources;
mod span;
mod stack;

pub use collation::{CollationCtype, CollationLocale, RegexCollation};
pub use error::{ErrorMessage, RegexError};
pub use flags::{
    CompileFlags, REG_ADVANCED, REG_ADVF, REG_ASSERT, REG_BADBR, REG_BADOPT, REG_BADPAT,
    REG_BADRPT, REG_BASIC, REG_EBRACE, REG_EBRACK, REG_ECOLLATE, REG_ECOLORS, REG_ECTYPE,
    REG_EESCAPE, REG_EPAREN, REG_ERANGE, REG_ESPACE, REG_ESUBREG, REG_ETOOBIG, REG_EXPANDED,
    REG_EXTENDED, REG_ICASE, REG_INVARG, REG_MIXED, REG_NEWLINE, REG_NLANCH, REG_NLSTOP,
    REG_NOMATCH, REG_NOSPEC, REG_NOSUB, REG_OKAY, REG_QUOTE,
};
pub use memory::MAX_ALLOC_SIZE;
pub use regex::Regex;
pub use resources::RegexResources;
pub use span::MatchSpan;
pub use stack::{MAX_STACK_DEPTH, STACK_DEPTH_SLOP};
