//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parse type modifiers once and retain any reconstructed qualified spelling with its owner.

use std::borrow::Cow;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

#[cfg(test)]
pub(crate) fn split_type_modifier(ty: &str) -> (Cow<'_, str>, Option<&str>) {
    let (base, modifier) = split_type_modifier_with_control(ty, &ProductionControl::uncontrolled())
        .expect("uncontrolled type modifier formatting cannot be cancelled or limited");
    (
        base.into_uncontrolled()
            .expect("uncontrolled type spelling has no reservation"),
        modifier,
    )
}

/// Preserve the existing modifier grammar; a suffix is copied only after its destination is admitted.
pub(crate) fn split_type_modifier_with_control<'a>(
    ty: &'a str,
    control: &ProductionControl<'_>,
) -> Result<(Produced<Cow<'a, str>>, Option<&'a str>), ValueRetentionError> {
    let (prefix, suffix, modifier) = match (ty.find('('), ty.rfind(')')) {
        (Some(open), Some(close)) if close > open => (
            ty[..open].trim_end(),
            ty[close + 1..].trim(),
            Some(ty[open + 1..close].trim()),
        ),
        _ => (ty, "", None),
    };
    let base = if suffix.is_empty() {
        control.finish(Cow::Borrowed(prefix), control.empty_reservation())?
    } else {
        let (text, memory) = control
            .format(format_args!("{prefix} {suffix}"))?
            .into_parts();
        control.finish(Cow::Owned(text), memory)?
    };
    Ok((base, modifier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn modifier_spelling_preserves_borrowing_and_suffix_grammar() {
        for (input, expected, modifier) in [
            ("numeric(10, 2)", "numeric", Some("10, 2")),
            ("time(3) with time zone", "time with time zone", Some("3")),
            (
                "interval day to second (4)",
                "interval day to second",
                Some("4"),
            ),
            ("varchar", "varchar", None),
            ("time(3", "time(3", None),
            ("time)3(", "time)3(", None),
        ] {
            let actual = split_type_modifier(input);
            assert_eq!(actual.0, expected);
            assert_eq!(actual.1, modifier);
        }
        assert!(matches!(split_type_modifier("text(4)").0, Cow::Borrowed(_)));
    }

    #[test]
    fn modifier_suffix_is_admitted_and_released() {
        let budget = MemoryBudget::new(1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let (base, modifier) =
            split_type_modifier_with_control("time(3) with time zone", &control).unwrap();
        assert_eq!(&**base, "time with time zone");
        assert_eq!(modifier, Some("3"));
        assert!(base.reserved_bytes() >= base.len());
        drop(base);
        assert_eq!(budget.used(), 0);
        let budget = MemoryBudget::new(0);
        let control = ProductionControl::new(&budget, &token, &token);
        assert!(split_type_modifier_with_control("time(3) with time zone", &control).is_err());
        assert!(split_type_modifier_with_control("numeric(3)", &control).is_ok());
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn either_modifier_owner_can_cancel_even_a_borrowed_name() {
        for original_cancelled in [true, false] {
            let budget = MemoryBudget::new(1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if original_cancelled {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&budget, &original, &invoking);
            assert!(matches!(
                split_type_modifier_with_control("text", &control),
                Err(ValueRetentionError::Cancelled(_))
            ));
            assert_eq!(budget.used(), 0);
        }
    }
}
