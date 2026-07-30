//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RPQ algebraic-identity property tests (Master Plan Section 2.4).
//!
//! The plan calls out three named identities the rewriter must hold:
//!
//! ```text
//! a | a       == a
//! (a*)*       == a*
//! a* | a      == a*
//! ```
//!
//! Existing unit tests in `rpq.rs` cover hand-picked instances; these
//! property tests assert the identities hold for *any* randomly drawn
//! expression `a`. Plus the standard rewrite-system invariant: simplify
//! is idempotent, i.e. `simplify(simplify(e)) == simplify(e)`.

use proptest::prelude::*;
use uqa_graph::{simplify, RegularPathExpr};

/// Bounded-depth strategy for arbitrary RPQ expressions. Without a
/// depth limit, `Box<dyn Strategy>` recursive generators can explode.
fn arb_expr() -> impl Strategy<Value = RegularPathExpr> {
    let leaf = "[a-z]{1,4}".prop_map(RegularPathExpr::label);
    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(l, r)| RegularPathExpr::concat(l, r)),
            (inner.clone(), inner.clone()).prop_map(|(l, r)| RegularPathExpr::alt(l, r)),
            inner.clone().prop_map(RegularPathExpr::star),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// Identity #1: `a | a == a`.
    #[test]
    fn alternation_idempotent(a in arb_expr()) {
        let lhs = simplify(&RegularPathExpr::alt(a.clone(), a.clone())).unwrap();
        let rhs = simplify(&a).unwrap();
        prop_assert_eq!(lhs, rhs);
    }

    /// Identity #2: `(a*)* == a*`.
    #[test]
    fn star_of_star_collapses(a in arb_expr()) {
        let inner = RegularPathExpr::star(a.clone());
        let nested = RegularPathExpr::star(inner);
        let just_one = RegularPathExpr::star(a);
        prop_assert_eq!(simplify(&nested).unwrap(), simplify(&just_one).unwrap());
    }

    /// Identity #3: `a* | a == a*` (and the symmetric `a | a* == a*`).
    #[test]
    fn star_subsumes_label(a in arb_expr()) {
        let star_a = RegularPathExpr::star(a.clone());
        let lhs = simplify(&RegularPathExpr::alt(star_a.clone(), a.clone())).unwrap();
        let rhs = simplify(&star_a.clone()).unwrap();
        prop_assert_eq!(lhs, rhs);

        let lhs_swap = simplify(&RegularPathExpr::alt(a, star_a.clone())).unwrap();
        prop_assert_eq!(lhs_swap, simplify(&star_a).unwrap());
    }

    /// `simplify` is idempotent: applying it a second time changes
    /// nothing.
    #[test]
    fn simplify_idempotent(e in arb_expr()) {
        let once = simplify(&e).unwrap();
        let twice = simplify(&once).unwrap();
        prop_assert_eq!(once, twice);
    }

    /// `Concat(a*, a*)` collapses to `a*` for any `a` (the
    /// "star + star of same expr" rule the simplifier uses).
    #[test]
    fn concat_of_two_same_stars_collapses(a in arb_expr()) {
        let star_a = RegularPathExpr::star(a);
        let concat = RegularPathExpr::concat(star_a.clone(), star_a.clone());
        prop_assert_eq!(simplify(&concat).unwrap(), simplify(&star_a).unwrap());
    }
}
