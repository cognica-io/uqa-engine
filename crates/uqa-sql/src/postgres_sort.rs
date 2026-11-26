//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL`'s ordering of equal elements affects the visible spelling of range bounds and the retained weight of a `tsvector` position. This fallible adaptation follows `PostgreSQL` 18's `src/include/lib/sort_template.h`. See `THIRD-PARTY/POSTGRESQL-SORT-NOTICE.txt` for attribution and terms.

use crate::error::Result;
use std::cmp::Ordering;
use uqa_core::memory::ProductionControl;

pub(crate) fn sort_by<T>(
    mut values: &mut [T],
    control: &ProductionControl<'_>,
    by: &impl Fn(&T, &T) -> Result<Ordering>,
) -> Result<()> {
    let compare = |left: &T, right: &T| {
        control.check()?;
        by(left, right)
    };
    loop {
        control.check()?;
        let count = values.len();
        if count < 7 {
            for index in 1..count {
                let mut position = index;
                while position > 0 && compare(&values[position - 1], &values[position])?.is_gt() {
                    values.swap(position - 1, position);
                    position -= 1;
                }
            }
            return Ok(());
        }
        let mut ordered = true;
        for index in 1..count {
            if compare(&values[index - 1], &values[index])?.is_gt() {
                ordered = false;
                break;
            }
        }
        if ordered {
            return Ok(());
        }
        let middle = pivot(values, &compare)?;
        values.swap(0, middle);
        let (mut lower_equal, mut lower_scan) = (1, 1);
        let (mut upper_scan, mut upper_equal) = (count - 1, count - 1);
        loop {
            while lower_scan <= upper_scan {
                let order = compare(&values[lower_scan], &values[0])?;
                if order.is_gt() {
                    break;
                }
                if order.is_eq() {
                    values.swap(lower_equal, lower_scan);
                    lower_equal += 1;
                }
                lower_scan += 1;
            }
            while lower_scan <= upper_scan {
                let order = compare(&values[upper_scan], &values[0])?;
                if order.is_lt() {
                    break;
                }
                if order.is_eq() {
                    values.swap(upper_scan, upper_equal);
                    upper_equal -= 1;
                }
                upper_scan -= 1;
            }
            if lower_scan > upper_scan {
                break;
            }
            values.swap(lower_scan, upper_scan);
            lower_scan += 1;
            upper_scan -= 1;
        }
        let lower_count = lower_scan - lower_equal;
        let upper_count = upper_equal - upper_scan;
        let swap_count = lower_equal.min(lower_count);
        swap_blocks(values, 0, lower_scan - swap_count, swap_count, control)?;
        let swap_count = upper_count.min(count - upper_equal - 1);
        swap_blocks(values, lower_scan, count - swap_count, swap_count, control)?;
        // Recurse only into the smaller partition: each recursive slice has at most half the previous length.
        if lower_count <= upper_count {
            if lower_count > 1 {
                sort_by(&mut values[..lower_count], control, by)?;
            }
            if upper_count <= 1 {
                return Ok(());
            }
            values = &mut values[count - upper_count..];
        } else {
            if upper_count > 1 {
                sort_by(&mut values[count - upper_count..], control, by)?;
            }
            if lower_count <= 1 {
                return Ok(());
            }
            values = &mut values[..lower_count];
        }
    }
}

fn pivot<T>(values: &[T], compare: &impl Fn(&T, &T) -> Result<Ordering>) -> Result<usize> {
    let count = values.len();
    let mut middle = count / 2;
    if count > 7 {
        let mut first = 0;
        let mut last = count - 1;
        if count > 40 {
            let step = count / 8;
            first = median(values, [first, first + step, first + 2 * step], compare)?;
            middle = median(values, [middle - step, middle, middle + step], compare)?;
            last = median(values, [last - 2 * step, last - step, last], compare)?;
        }
        middle = median(values, [first, middle, last], compare)?;
    }
    Ok(middle)
}

fn median<T>(
    values: &[T],
    [a, b, c]: [usize; 3],
    compare: &impl Fn(&T, &T) -> Result<Ordering>,
) -> Result<usize> {
    Ok(if compare(&values[a], &values[b])?.is_lt() {
        if compare(&values[b], &values[c])?.is_lt() {
            b
        } else if compare(&values[a], &values[c])?.is_lt() {
            c
        } else {
            a
        }
    } else if compare(&values[b], &values[c])?.is_gt() {
        b
    } else if compare(&values[a], &values[c])?.is_lt() {
        a
    } else {
        c
    })
}

fn swap_blocks<T>(
    values: &mut [T],
    left: usize,
    right: usize,
    count: usize,
    control: &ProductionControl<'_>,
) -> Result<()> {
    for index in 0..count {
        control.check()?;
        values.swap(left + index, right + index);
    }
    Ok(())
}
