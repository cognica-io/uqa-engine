//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resource hooks with fault injection, and `PostgreSQL`'s SQL-level option parsing.

use std::cell::RefCell;
use std::rc::Rc;

use uqa_pg_regex::{
    CompileFlags, RegexCollation, RegexResources, REG_ADVANCED, REG_EXPANDED, REG_EXTENDED,
    REG_ICASE, REG_NEWLINE, REG_NLANCH, REG_NLSTOP, REG_QUOTE,
};

/// Resource hooks that account every byte and can refuse, cancel or limit at a chosen point.
#[derive(Debug, Default)]
pub struct Budget {
    /// Bytes currently reserved and not yet released.
    pub live: usize,
    /// Largest `live` value.
    pub peak: usize,
    /// Number of reservation requests, including refused ones.
    pub reservations: usize,
    /// Number of interrupt checks.
    pub interrupts: usize,
    /// Largest stack depth reported by the engine.
    pub deepest: usize,
    /// Refuse every reservation above this many live bytes.
    pub limit: Option<usize>,
    /// Refuse exactly the reservation with this zero-based index.
    pub refuse_reservation: Option<usize>,
    /// Request cancellation at the interrupt check with this zero-based index.
    pub cancel_interrupt: Option<usize>,
    /// Report the stack as too deep beyond this many bytes.
    pub stack_limit: Option<usize>,
    /// Panic at the reservation with this zero-based index.
    pub panic_reservation: Option<usize>,
    /// Panic at the interrupt check with this zero-based index.
    pub panic_interrupt: Option<usize>,
}

impl Budget {
    pub fn unlimited() -> Self {
        Self::default()
    }
}

impl RegexResources for Budget {
    fn reserve(&mut self, bytes: usize) -> bool {
        let index = self.reservations;
        self.reservations += 1;
        assert!(
            self.panic_reservation != Some(index),
            "reservation hook panic injected at {index}"
        );
        if self.refuse_reservation == Some(index)
            || self.limit.is_some_and(|limit| self.live + bytes > limit)
        {
            return false;
        }
        self.live += bytes;
        self.peak = self.peak.max(self.live);
        true
    }

    fn release(&mut self, bytes: usize) {
        self.live = self
            .live
            .checked_sub(bytes)
            .expect("the engine never releases more than it reserved");
    }

    fn cancel_requested(&mut self) -> bool {
        let index = self.interrupts;
        self.interrupts += 1;
        assert!(
            self.panic_interrupt != Some(index),
            "interrupt hook panic injected at {index}"
        );
        self.cancel_interrupt == Some(index)
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        self.deepest = self.deepest.max(depth);
        self.stack_limit.is_some_and(|limit| depth > limit)
    }
}

/// A [`Budget`] that the test can adjust while a compiled expression borrows the hooks.
#[derive(Clone, Default)]
pub struct SharedBudget(pub Rc<RefCell<Budget>>);

impl RegexResources for SharedBudget {
    fn reserve(&mut self, bytes: usize) -> bool {
        self.0.borrow_mut().reserve(bytes)
    }

    fn release(&mut self, bytes: usize) {
        self.0.borrow_mut().release(bytes);
    }

    fn cancel_requested(&mut self) -> bool {
        self.0.borrow_mut().cancel_requested()
    }

    fn stack_too_deep(&mut self, depth: usize) -> bool {
        self.0.borrow_mut().stack_too_deep(depth)
    }
}

pub fn code_points(text: &str) -> Vec<u32> {
    text.chars().map(u32::from).collect()
}

pub fn text(code_points: &[u32]) -> String {
    code_points
        .iter()
        .map(|code| char::from_u32(*code).expect("subjects hold Unicode scalar values"))
        .collect()
}

/// `PostgreSQL`'s `parse_re_flags` in `src/backend/utils/adt/regexp.c` for the options the reference uses; `g` is not part of the compile flags.
pub fn parse_re_flags(options: &str) -> CompileFlags {
    let mut flags = REG_ADVANCED;
    for option in options.chars() {
        match option {
            'b' => flags &= !(REG_ADVANCED | REG_EXTENDED | REG_QUOTE),
            'c' => flags &= !REG_ICASE,
            'e' => {
                // REG_ADVANCED includes REG_EXTENDED, so PostgreSQL 18.4's second step clears the bit the first one set and "e" compiles basic expressions; the reference records that behavior.
                flags |= REG_EXTENDED;
                flags &= !(REG_ADVANCED | REG_QUOTE);
            }
            'i' => flags |= REG_ICASE,
            'm' | 'n' => flags |= REG_NEWLINE,
            'p' => {
                flags |= REG_NLSTOP;
                flags &= !REG_NLANCH;
            }
            'q' => {
                flags |= REG_QUOTE;
                flags &= !(REG_ADVANCED | REG_EXTENDED);
            }
            's' => flags &= !REG_NEWLINE,
            't' => flags &= !REG_EXPANDED,
            'w' => {
                flags &= !REG_NLSTOP;
                flags |= REG_NLANCH;
            }
            'x' => flags |= REG_EXPANDED,
            other => panic!("the reference uses no option {other:?}"),
        }
    }
    flags
}

/// The collation named in a reference case, resolved through its catalog OID.
pub fn collation(name: &str) -> RegexCollation {
    let oid = match name {
        "C" => 950,
        "POSIX" => 951,
        "ucs_basic" => 962,
        "pg_c_utf8" => 811,
        "pg_unicode_fast" => 6411,
        "unicode" => 963,
        "default" => 100,
        other => panic!("the reference uses no collation {other:?}"),
    };
    RegexCollation::from_oid(oid).expect("predefined collations resolve")
}
