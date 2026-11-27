//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in coercions and the explicit cast generated with each SQL range declaration.

use super::super::helpers::rows::{int_value, row, str_value};
use crate::catalog::CatalogReadView;
use uqa_sql::{catalog::range_type::StoredRange, type_resolution::builtin_casts, ResultRow};

pub(in crate::catalog::projection) fn rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    builtin_casts()
        .chain(catalog.ranges().filter_map(StoredRange::cast))
        .map(|cast| {
            row([
                ("oid", int_value(i64::from(cast.oid))),
                ("castsource", int_value(i64::from(cast.source))),
                ("casttarget", int_value(i64::from(cast.target))),
                ("castfunc", int_value(i64::from(cast.function))),
                ("castcontext", str_value(char::from(cast.context))),
                ("castmethod", str_value(char::from(cast.method))),
            ])
        })
        .collect()
}
