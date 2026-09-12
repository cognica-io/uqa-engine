//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RPQ expression complexity estimation.

/// Count label nodes in an RPQ expression by parsing the source and walking
/// the AST. Label = 1, Concat/Alt = sum, KleeneStar =
/// inner * 2, Bounded = inner * max_hops. Falls back to 1 when the
/// source can't be parsed.
pub(super) fn rpq_label_count(source: &str) -> usize {
    match uqa_core::rpq::parse_rpq(source) {
        Ok(expr) => count_rpq_labels(&expr).max(1),
        Err(_) => 1,
    }
}

fn count_rpq_labels(expr: &uqa_core::rpq::RegularPathExpr) -> usize {
    use uqa_core::rpq::RegularPathExpr;
    match expr {
        RegularPathExpr::Label(_) => 1,
        RegularPathExpr::Concat(l, r) | RegularPathExpr::Alternation(l, r) => {
            count_rpq_labels(l) + count_rpq_labels(r)
        }
        RegularPathExpr::KleeneStar(inner) => count_rpq_labels(inner).saturating_mul(2),
        RegularPathExpr::Bounded { inner, max, .. } => {
            count_rpq_labels(inner).saturating_mul(usize::try_from(*max).unwrap_or(usize::MAX))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rpq_label_count;

    #[test]
    fn complexity_counts_nested_repetition_and_keeps_parse_failure_fallback() {
        for (source, count) in [
            ("(a/b|c{2,4})*", 12),
            ("a{0,0}", 1),
            ("a/", 1),
            ("a{4,2}", 1),
        ] {
            assert_eq!(rpq_label_count(source), count, "{source}");
        }
    }
}
