//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View definition publication within the active catalog transaction.
pub mod context;
mod materialized;
mod publication;
mod registration;
pub use materialized::{refresh_materialized_view, register_materialized_view_plan};
pub use registration::{register_view, register_view_plan};
use uqa_sql::plan::QueryPlan;

pub struct ViewRegistration<'a> {
    pub name: &'a str,
    pub column_names: &'a [String],
    pub plan: QueryPlan,
    pub or_replace: bool,
    pub persistence: uqa_sql::ast::RelationPersistence,
    pub options: &'a [(String, String)],
    pub params: &'a [uqa_sql::SQLParam],
}

pub struct MaterializedViewRegistration<'a> {
    pub name: &'a str,
    pub column_names: &'a [String],
    pub plan: QueryPlan,
    pub if_not_exists: bool,
    pub with_no_data: bool,
    pub options: &'a [(String, String)],
    pub params: &'a [uqa_sql::SQLParam],
}
