//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_opclass` rows from the SQL operator class catalog.

use super::super::helpers::rows::{bool_value, int_value, row, str_value};
use uqa_sql::{catalog::index::operator_classes::builtin_operator_classes, ResultRow};

pub(in crate::catalog::projection) fn rows() -> Vec<ResultRow> {
    builtin_operator_classes()
        .map(|class| {
            row([
                ("oid", int_value(i64::from(class.oid))),
                ("opcmethod", int_value(i64::from(class.method))),
                ("opcname", str_value(class.name)),
                ("opcnamespace", int_value(11)),
                ("opcowner", int_value(10)),
                ("opcfamily", int_value(i64::from(class.family))),
                ("opcintype", int_value(i64::from(class.input))),
                ("opcdefault", bool_value(class.default)),
                ("opckeytype", int_value(i64::from(class.key))),
            ])
        })
        .collect()
}
