//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind an independently executable subquery to a table's index at the first request for rows.

use crate::catalog::{CatalogReadView, CatalogTableSnapshot};
use crate::query::{
    index_candidates::capture_predicate, set_projection::SetFunctionRuntime,
    sources::SourceContext, CteScope, SelectedIds,
};
use crate::scalar::plan::PhysicalOuterRow;
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    CancellationToken, Predicate, Value,
};
use uqa_sql::{
    catalog::index::lookup::{self, EqualityDomain, SubqueryKeys},
    expr::RowLookup,
    SQLError, SQLParam,
};

type Probe<'a> = Box<dyn FnOnce() -> Result<Option<SelectedIds>, SQLError> + Send + 'a>;

pub(in crate::query) struct Runtime<'a, 'f> {
    field: &'f str,
    scope: Arc<dyn SetFunctionRuntime + 'a>,
    pub(super) diagnostics: &'a crate::query::diagnostics::QueryDiagnostics,
    reads: &'a dyn crate::query::table_read::QueryTableAccess,
    params: &'a [SQLParam],
    keys: SubqueryKeys,
    domain: Option<EqualityDomain>,
    command_visible: bool,
    bytes: usize,
    cancellation: CancellationToken,
}

impl<'a, 'f> Runtime<'a, 'f> {
    pub(in crate::query) fn new<S: Clone + 'static>(
        context: &SourceContext<'a, S>,
        ctes: &CteScope<S>,
        params: &'a [SQLParam],
        field: &'f str,
        keys: SubqueryKeys,
    ) -> Result<Self, SQLError> {
        let scope = context.relational.expression_scope(ctes)?;
        let plan = scope.subquery_plans().get(keys.slot()).ok_or_else(|| {
            SQLError::Internal("missing subquery in index scan's statement arena".into())
        })?;
        let resources = context.ctes.production_resources()?;
        let domain = crate::query::binding::subquery_key_domain(
            context.ctes.routines,
            plan,
            params,
            ctes,
            &resources.control(),
        )?;
        Ok(Self {
            field,
            scope,
            diagnostics: context.scans.runtime.diagnostics,
            reads: context.scans.tables,
            params,
            keys,
            domain,
            command_visible: ctes.reads_command_overlay(),
            bytes: context.scans.runtime.work_mem_bytes()?,
            cancellation: context.scans.runtime.cancellation_token(),
        })
    }

    pub(super) fn bind(
        &self,
        table: &CatalogTableSnapshot,
        maps_integer_keys: bool,
        name: &str,
        catalog: &CatalogReadView,
        visible: &dyn Fn(&str) -> String,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Probe<'a>>, SQLError> {
        let Some(column) = table
            .columns
            .iter()
            .find(|column| visible(&column.name) == self.field)
        else {
            return Ok(None);
        };
        if self.domain.is_none()
            || EqualityDomain::of(&column.ty, column.collation_oid) != self.domain
            || !lookup::is_unique_column(
                name,
                column,
                &table.columns,
                &table.keys,
                catalog.catalog_indexes(),
            )?
        {
            return Ok(None);
        }
        let name = control.copy_text(name)?;
        let integer_identity = maps_integer_keys && column.primary_key && column.ty.is_integer();
        let field = control.copy_text(&column.name)?;
        let scope = self.scope.clone();
        let (keys, params, reads, command_visible, bytes) = (
            self.keys,
            self.params,
            self.reads,
            self.command_visible,
            self.bytes,
        );
        let cancellation = self.cancellation.clone();
        let resources = control.capture();
        Ok(Some(Box::new(move || {
            let slot = keys.slot();
            let plan = &scope.subquery_plans()[slot];
            let probe = |key: Value| {
                if integer_identity {
                    if let Some(id) = uqa_sql::semantics::key_identity::key_document_id(&key) {
                        return SelectedIds::from_sorted(vec![id], bytes, &cancellation).map(Some);
                    }
                }
                capture_predicate(
                    reads,
                    &name,
                    &field,
                    &Predicate::Equals(key),
                    command_visible,
                    bytes,
                    &cancellation,
                )
            };
            match keys {
                SubqueryKeys::Scalar(_) => {
                    let value = scope.scalar_subquery_value(
                        slot,
                        plan,
                        PhysicalOuterRow::Absent,
                        params,
                    )?;
                    if value == Value::Null {
                        return SelectedIds::from_sorted(Vec::new(), bytes, &cancellation)
                            .map(Some);
                    }
                    probe(value)
                }
                SubqueryKeys::Membership(_) => {
                    let Some(input) = scope.index_membership_input(slot, plan, params)? else {
                        return Ok(None);
                    };
                    let Some(keys) = membership_keys(input, bytes, &resources.control())? else {
                        return Ok(None);
                    };
                    let (keys, _key_memory) = keys.into_parts();
                    let mut complete = true;
                    let ids = SelectedIds::capture_visit(bytes, &cancellation, |emit| {
                        for key in keys {
                            let Some(ids) = probe(key)? else {
                                complete = false;
                                break;
                            };
                            for id in ids.cursor() {
                                emit(id?)?;
                            }
                        }
                        Ok(())
                    })?;
                    Ok(complete.then_some(ids))
                }
            }
        })))
    }
}

fn membership_keys(
    input: crate::SubqueryResult,
    bytes: usize,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Value>>>, SQLError> {
    let mut keys = ProductionVec::new(*control);
    let mut payload = 0_usize;
    for row in input.rows {
        control.check()?;
        let row = row?;
        let Some(key) = row.positional_column(0) else {
            continue;
        };
        if *key == Value::Null || keys.contains(key) {
            continue;
        }
        payload = payload
            .saturating_add(size_of::<Value>())
            .saturating_add(key.retained_payload_bytes_with_control(control)?);
        if keys.len() == lookup::MAX_MEMBERSHIP_PROBES || payload > bytes {
            return Ok(None);
        }
        keys.push_produced(control.copy_value(key)?)?;
    }
    Ok(Some(keys.finish()?))
}
