//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range normalization preserves `PostgreSQL`'s equal-bound permutation, which affects numeric output spelling. This fallible, cancellable Rust adaptation follows `PostgreSQL` 18's `src/include/lib/sort_template.h`; attribution and upstream terms are in `THIRD-PARTY/POSTGRESQL-SORT-NOTICE.txt`.

use super::{CanonicalRange, Ordering, ProductionControl, RangeConstruction, Result};

pub(super) fn sort<O: RangeConstruction + ?Sized>(
    mut ranges: &mut [CanonicalRange<O::Identity>],
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<()> {
    loop {
        control.check()?;
        let count = ranges.len();
        if count < 7 {
            for index in 1..count {
                let mut position = index;
                while position > 0
                    && compare(ranges, position - 1, position, operations, control)?.is_gt()
                {
                    ranges.swap(position - 1, position);
                    position -= 1;
                }
            }
            return Ok(());
        }
        let mut ordered = true;
        for index in 1..count {
            if compare(ranges, index - 1, index, operations, control)?.is_gt() {
                ordered = false;
                break;
            }
        }
        if ordered {
            return Ok(());
        }
        let middle = pivot(ranges, operations, control)?;
        ranges.swap(0, middle);
        let (mut lower_equal, mut lower_scan) = (1, 1);
        let (mut upper_scan, mut upper_equal) = (count - 1, count - 1);
        loop {
            while lower_scan <= upper_scan {
                let order = compare(ranges, lower_scan, 0, operations, control)?;
                if order.is_gt() {
                    break;
                }
                if order.is_eq() {
                    ranges.swap(lower_equal, lower_scan);
                    lower_equal += 1;
                }
                lower_scan += 1;
            }
            while lower_scan <= upper_scan {
                let order = compare(ranges, upper_scan, 0, operations, control)?;
                if order.is_lt() {
                    break;
                }
                if order.is_eq() {
                    ranges.swap(upper_scan, upper_equal);
                    upper_equal -= 1;
                }
                upper_scan -= 1;
            }
            if lower_scan > upper_scan {
                break;
            }
            ranges.swap(lower_scan, upper_scan);
            lower_scan += 1;
            upper_scan -= 1;
        }
        let lower_count = lower_scan - lower_equal;
        let upper_count = upper_equal - upper_scan;
        let swap_count = lower_equal.min(lower_count);
        swap_blocks(ranges, 0, lower_scan - swap_count, swap_count, control)?;
        let swap_count = upper_count.min(count - upper_equal - 1);
        swap_blocks(ranges, lower_scan, count - swap_count, swap_count, control)?;
        // Recurse only into the smaller partition: each recursive slice has at most half the previous length.
        if lower_count <= upper_count {
            if lower_count > 1 {
                sort(&mut ranges[..lower_count], operations, control)?;
            }
            if upper_count <= 1 {
                return Ok(());
            }
            ranges = &mut ranges[count - upper_count..];
        } else {
            if upper_count > 1 {
                sort(&mut ranges[count - upper_count..], operations, control)?;
            }
            if lower_count <= 1 {
                return Ok(());
            }
            ranges = &mut ranges[..lower_count];
        }
    }
}

fn pivot<O: RangeConstruction + ?Sized>(
    ranges: &[CanonicalRange<O::Identity>],
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<usize> {
    let count = ranges.len();
    let mut middle = count / 2;
    if count > 7 {
        let mut first = 0;
        let mut last = count - 1;
        if count > 40 {
            let step = count / 8;
            first = median(
                ranges,
                [first, first + step, first + 2 * step],
                operations,
                control,
            )?;
            middle = median(
                ranges,
                [middle - step, middle, middle + step],
                operations,
                control,
            )?;
            last = median(
                ranges,
                [last - 2 * step, last - step, last],
                operations,
                control,
            )?;
        }
        middle = median(ranges, [first, middle, last], operations, control)?;
    }
    Ok(middle)
}

fn compare<O: RangeConstruction + ?Sized>(
    ranges: &[CanonicalRange<O::Identity>],
    left: usize,
    right: usize,
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    ranges[left].cmp_by_with_control(&ranges[right], control, |left, right, control| {
        operations.compare_bounds(left, right, control)
    })
}

fn median<O: RangeConstruction + ?Sized>(
    ranges: &[CanonicalRange<O::Identity>],
    [a, b, c]: [usize; 3],
    operations: &O,
    control: &ProductionControl<'_>,
) -> Result<usize> {
    Ok(if compare(ranges, a, b, operations, control)?.is_lt() {
        if compare(ranges, b, c, operations, control)?.is_lt() {
            b
        } else if compare(ranges, a, c, operations, control)?.is_lt() {
            c
        } else {
            a
        }
    } else if compare(ranges, b, c, operations, control)?.is_gt() {
        b
    } else if compare(ranges, a, c, operations, control)?.is_lt() {
        a
    } else {
        c
    })
}

fn swap_blocks<I>(
    ranges: &mut [CanonicalRange<I>],
    left: usize,
    right: usize,
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<()> {
    for index in 0..count {
        control.check()?;
        ranges.swap(left + index, right + index);
    }
    Ok(())
}
