//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected endpoint input, ordering and canonicalization share admitted range constructors.

use super::{
    CanonicalMultirange, CanonicalRange, Ordering, ProductionControl, Result, SQLError, Value,
};
use uqa_core::memory::{MemoryReservation, Produced, ProductionVec};

mod ordering;

/// Operations selected by the range definition, independently of its endpoint layout. Invocation state and effects belong to the caller; observation never uses this interface.
pub trait RangeConstruction {
    type Identity: Copy + Eq;

    fn identity(&self) -> Self::Identity;

    /// Whether the definition selected a canonical function. Adjacency may invoke it on the gap between disjoint bounds.
    fn has_canonical_function(&self) -> bool;

    /// Decode one present endpoint, including its selected input and domain checks.
    fn input_bound(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>>;

    fn compare_bounds(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering>;

    /// Invoke the optional canonical function for a nonempty range. `None` retains the borrowed input unchanged; a replacement must own all its allocations under `control`. Returning SQL NULL is an invocation error, not `None`. The input's owner remains live throughout the call and cannot escape with an empty replacement lease.
    fn canonicalize(
        &self,
        _range: &CanonicalRange<Self::Identity>,
        _control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CanonicalRange<Self::Identity>>>> {
        Ok(None)
    }
}

// The complete live value precedes its lease, including while endpoints move between work buffers.
pub(super) struct RangeWorkspace<T> {
    pub(super) value: T,
    pub(super) memory: Option<MemoryReservation>,
}

impl<T> RangeWorkspace<T> {
    pub(super) fn from_produced(value: Produced<T>) -> Self {
        let (value, memory) = value.into_parts();
        Self { value, memory }
    }
}

/// Validate a range constructor's explicit flags argument. The two-argument constructor supplies `Some("[)")`; SQL NULL remains an error.
pub fn range_bound_flags(flags: Option<&str>) -> Result<(bool, bool)> {
    let Some(flags) = flags else {
        return Err(SQLError::Routine {
            sqlstate: "22000".into(),
            message: "range constructor flags argument must not be null".into(),
        });
    };
    match flags.as_bytes() {
        [lower @ (b'[' | b'('), upper @ (b']' | b')')] => Ok((*lower == b'[', *upper == b']')),
        _ => Err(SQLError::Diagnostic {
            sqlstate: "42601".into(),
            message: "invalid range bound flags".into(),
            detail: None,
            hint: Some("Valid values are \"[]\", \"[)\", \"(]\", and \"()\".".into()),
        }),
    }
}

/// Parse complete range syntax, then input the lower and upper endpoints in order. Already empty input does not invoke endpoint or canonical functions.
pub fn parse_range_with_operations<O: RangeConstruction + ?Sized>(
    text: &str,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalRange<O::Identity>>> {
    control.check()?;
    let Some(input) = super::input::range(text, control)? else {
        return Ok(control.finish(
            CanonicalRange::empty(operations.identity()),
            control.empty_reservation(),
        )?);
    };
    let lower = parse_bound(input.lower, operations, control)?;
    let upper = parse_bound(input.upper, operations, control)?;
    range_from_bounds_with_operations(
        lower,
        upper,
        input.lower_inclusive,
        input.upper_inclusive,
        operations,
        control,
    )
}

/// Construct from already coerced, admitted, non-NULL endpoints; `None` represents an unbounded endpoint. This does not repeat endpoint input or domain checks. Bounds are validated before the optional canonical call; the canonical result is returned without a second call or validation pass.
pub fn range_from_bounds_with_operations<O: RangeConstruction + ?Sized>(
    lower: Produced<Option<Value>>,
    upper: Produced<Option<Value>>,
    lower_inclusive: bool,
    upper_inclusive: bool,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalRange<O::Identity>>> {
    control.check()?;
    let lower_inclusive = lower_inclusive && lower.is_some();
    let upper_inclusive = upper_inclusive && upper.is_some();
    let (lower, lower_memory) = lower.into_parts();
    let (upper, upper_memory) = upper.into_parts();
    let mut owned = RangeWorkspace {
        value: CanonicalRange {
            subtype: operations.identity(),
            lower,
            upper,
            lower_inclusive,
            upper_inclusive,
            empty: false,
        },
        memory: control.combine(lower_memory, upper_memory),
    };
    if bounds_are_empty(&owned.value, operations, control)? {
        owned.value = CanonicalRange::empty(operations.identity());
    } else if let Some(replacement) = canonicalize(&owned.value, operations, control)? {
        return Ok(replacement);
    }
    Ok(control.finish(owned.value, owned.memory)?)
}

pub(super) fn bounds_are_empty<O: RangeConstruction + ?Sized>(
    range: &CanonicalRange<O::Identity>,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    Ok(match (&range.lower, &range.upper) {
        (Some(lower), Some(upper)) => match operations.compare_bounds(lower, upper, control)? {
            Ordering::Greater => {
                return Err(SQLError::Routine {
                    sqlstate: "22000".into(),
                    message: "range lower bound must be less than or equal to range upper bound"
                        .into(),
                })
            }
            Ordering::Equal => !(range.lower_inclusive && range.upper_inclusive),
            Ordering::Less => false,
        },
        _ => false,
    })
}

fn canonicalize<O: RangeConstruction + ?Sized>(
    range: &CanonicalRange<O::Identity>,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<CanonicalRange<O::Identity>>>> {
    control.check()?;
    if !operations.has_canonical_function() {
        return Ok(None);
    }
    let Some(replacement) = operations.canonicalize(range, control)? else {
        control.check()?;
        return Ok(None);
    };
    require_identity(&replacement, operations.identity())?;
    let (value, memory) = replacement.into_parts();
    Ok(Some(control.finish(value, memory)?))
}

/// Parse each member through the same range constructor before sorting and joining overlapping or adjacent members.
pub fn parse_multirange_with_operations<O: RangeConstruction + ?Sized>(
    text: &str,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange<O::Identity>>> {
    control.check()?;
    let mut input = super::input::MultirangeInput::new(text, control)?;
    let mut ranges = ProductionVec::new(*control);
    while let Some(item) = input.next_range(control)? {
        let range = parse_range_with_operations(item, operations, control)?;
        if !range.empty {
            ranges.push_produced(range)?;
        }
    }
    multirange_from_ranges_with_operations(ranges.finish()?, operations, control)
}

/// Normalize already constructed members without repeating their input or canonical calls. Every union invokes the selected canonical operation once, as range construction does.
pub fn multirange_from_ranges_with_operations<O: RangeConstruction + ?Sized>(
    ranges: Produced<Vec<CanonicalRange<O::Identity>>>,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<Produced<CanonicalMultirange<O::Identity>>> {
    let (ranges, memory) = ranges.into_parts();
    normalize_ranges(ranges, operations, control, memory)
}

pub(super) fn normalize_ranges<O: RangeConstruction + ?Sized>(
    ranges: impl IntoIterator<Item = CanonicalRange<O::Identity>>,
    operations: &O,
    control: &ProductionControl<'_>,
    memory: Option<MemoryReservation>,
) -> Result<Produced<CanonicalMultirange<O::Identity>>> {
    let mut collecting = RangeWorkspace {
        value: (ranges.into_iter(), ProductionVec::new(*control)),
        memory,
    };
    let (input, ordered) = &mut collecting.value;
    for range in input {
        control.check()?;
        require_identity(&range, operations.identity())?;
        ordered.push_produced(control.finish(range, control.empty_reservation())?)?;
    }
    let (input, ordered) = collecting.value;
    drop(input);
    let mut ordered = RangeWorkspace::from_produced(ordered.finish()?);
    ordered.memory = control.combine(collecting.memory, ordered.memory);
    ordering::sort(&mut ordered.value, operations, control)?;
    let mut normalized = ProductionVec::new(*control);
    normalized.reserve(ordered.value.len())?;
    let normalized = RangeWorkspace::from_produced(normalized.finish()?);
    // The current canonical replacement can be superseded. Keep its lease separate from completed members, and drop live values before this lease on failure.
    let mut active_memory = control.empty_reservation();
    let mut merging = RangeWorkspace {
        value: (ordered.value.into_iter(), normalized.value),
        memory: control.combine(ordered.memory, normalized.memory),
    };
    let (input, normalized) = &mut merging.value;
    for range in input {
        control.check()?;
        if range.empty {
            continue;
        }
        let joins_previous = normalized
            .last()
            .map(|previous| joins(previous, &range, operations, control))
            .transpose()?
            .unwrap_or(false);
        if joins_previous {
            let previous = normalized.pop().expect("previous range");
            if previous.empty {
                drop(previous);
                active_memory = control.empty_reservation();
                normalized.push(range);
                continue;
            }
            let merged = merge_owned(previous, range, operations, control)?;
            if bounds_are_empty(&merged, operations, control)? {
                drop(merged);
                active_memory = control.empty_reservation();
                normalized.push(CanonicalRange::empty(operations.identity()));
            } else if let Some(replacement) = canonicalize(&merged, operations, control)? {
                let (value, memory) = replacement.into_parts();
                drop(merged);
                active_memory = memory;
                normalized.push(value);
            } else {
                normalized.push(merged);
            }
        } else {
            merging.memory = control.combine(merging.memory.take(), active_memory.take());
            active_memory = control.empty_reservation();
            // Capacity was admitted for every input member before normalization.
            normalized.push(range);
        }
    }
    let (input, normalized) = merging.value;
    drop(input);
    Ok(control.finish(
        CanonicalMultirange {
            subtype: operations.identity(),
            ranges: normalized,
        },
        control.combine(merging.memory, active_memory),
    )?)
}

fn require_identity<I: PartialEq>(range: &CanonicalRange<I>, identity: I) -> Result<()> {
    if range.subtype != identity {
        return Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: "range types do not match".into(),
        });
    }
    Ok(())
}

fn parse_bound<O: RangeConstruction + ?Sized>(
    input: Option<super::input::BoundInput<'_>>,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Value>>> {
    let Some(input) = input else {
        return Ok(control.finish(None, control.empty_reservation())?);
    };
    let (text, memory) = input.decode(control)?.into_parts();
    let source = RangeWorkspace {
        value: Value::Str(text),
        memory,
    };
    let (value, memory) = operations.input_bound(&source.value, control)?.into_parts();
    Ok(control.finish(Some(value), memory)?)
}

fn joins<O: RangeConstruction + ?Sized>(
    left: &CanonicalRange<O::Identity>,
    right: &CanonicalRange<O::Identity>,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    // A canonical callback can return empty; union with it preserves the next member without another callback.
    if left.empty {
        return Ok(true);
    }
    Ok(match (left.upper(), right.lower()) {
        (None, _) | (_, None) => true,
        (Some(upper), Some(lower)) => match operations.compare_bounds(upper, lower, control)? {
            Ordering::Less if operations.has_canonical_function() => {
                let gap_lower = copy_bound(Some(upper), control)?;
                let gap_upper = copy_bound(Some(lower), control)?;
                range_from_bounds_with_operations(
                    gap_lower,
                    gap_upper,
                    !left.upper_inclusive,
                    !right.lower_inclusive,
                    operations,
                    control,
                )?
                .empty
            }
            Ordering::Less => false,
            Ordering::Greater => true,
            Ordering::Equal => left.upper_inclusive || right.lower_inclusive,
        },
    })
}

pub(super) fn copy_bound(
    value: Option<&Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Value>>> {
    match value {
        Some(value) => {
            let (value, memory) = control.copy_value(value)?.into_parts();
            Ok(control.finish(Some(value), memory)?)
        }
        None => Ok(control.finish(None, control.empty_reservation())?),
    }
}

fn merge_owned<O: RangeConstruction + ?Sized>(
    mut left: CanonicalRange<O::Identity>,
    mut right: CanonicalRange<O::Identity>,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<CanonicalRange<O::Identity>> {
    if left.empty {
        return Ok(right);
    }
    // PostgreSQL's union chooses the right operand when equal bounds have distinct physical spellings.
    if super::comparison::compare_lower_bounds_by(&left, &right, control, |a, b, control| {
        operations.compare_bounds(a, b, control)
    })? != Ordering::Less
    {
        left.lower = right.lower.take();
        left.lower_inclusive = right.lower_inclusive;
    }
    match (left.upper(), right.upper()) {
        (None, _) => {}
        (_, None) => {
            left.upper = None;
            left.upper_inclusive = false;
        }
        (Some(a), Some(b)) => match operations
            .compare_bounds(a, b, control)?
            .then_with(|| left.upper_inclusive.cmp(&right.upper_inclusive))
        {
            Ordering::Less | Ordering::Equal => {
                left.upper = right.upper.take();
                left.upper_inclusive = right.upper_inclusive;
            }
            Ordering::Greater => {}
        },
    }
    Ok(left)
}

#[cfg(test)]
mod tests;
