//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_depend` and `pg_shdepend` rows.

use crate::catalog::projection::helpers::rows::{int_value, row, str_value};
use uqa_sql::catalog::dependencies::{Dependency, SharedDependency};
use uqa_sql::ResultRow;

pub(super) fn depend_row(dependency: &Dependency) -> ResultRow {
    row([
        (
            "classid",
            int_value(i64::from(dependency.dependent.class_id)),
        ),
        (
            "objid",
            int_value(i64::from(dependency.dependent.object_id)),
        ),
        (
            "objsubid",
            int_value(i64::from(dependency.dependent.sub_id)),
        ),
        (
            "refclassid",
            int_value(i64::from(dependency.referenced.class_id)),
        ),
        (
            "refobjid",
            int_value(i64::from(dependency.referenced.object_id)),
        ),
        (
            "refobjsubid",
            int_value(i64::from(dependency.referenced.sub_id)),
        ),
        ("deptype", str_value(dependency.kind.code())),
    ])
}

pub(super) fn shared_depend_row(dependency: &SharedDependency) -> ResultRow {
    row([
        ("dbid", int_value(i64::from(dependency.database))),
        (
            "classid",
            int_value(i64::from(dependency.dependent.class_id)),
        ),
        (
            "objid",
            int_value(i64::from(dependency.dependent.object_id)),
        ),
        (
            "objsubid",
            int_value(i64::from(dependency.dependent.sub_id)),
        ),
        (
            "refclassid",
            int_value(i64::from(dependency.referenced.class_id)),
        ),
        (
            "refobjid",
            int_value(i64::from(dependency.referenced.object_id)),
        ),
        ("deptype", str_value(dependency.kind.code())),
    ])
}
