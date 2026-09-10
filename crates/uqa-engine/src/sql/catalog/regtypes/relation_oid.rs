//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation OID resolution for SQL names and already-bound dependency identities.

use super::{
    catalog_index_relations, numeric_regobject_oid, qualified_name, relation_name,
    resolve_virtual_regclass, Engine, NumericRegobjectOid, SQLError,
};

pub(super) fn lookup_regclass_oid(engine: &Engine, name: &str) -> Result<Option<i64>, SQLError> {
    match numeric_regobject_oid(name) {
        NumericRegobjectOid::Valid(oid) => return Ok(Some(oid)),
        NumericRegobjectOid::InvalidSyntax | NumericRegobjectOid::OutOfRange => return Ok(None),
        NumericRegobjectOid::NotNumeric => {}
    }
    let Some(names) = uqa_sql::parse_regobject_name(name) else {
        return Ok(None);
    };
    let (schema, local) = relation_name(&names)?;
    if let Some((oid, _, _)) = resolve_virtual_regclass(engine, schema, local)? {
        return Ok(Some(oid));
    }
    let reference = schema.map_or_else(
        || uqa_sql::expr::quote_ident(local),
        |schema| qualified_name(schema, local),
    );
    let Some((canonical, kind)) = engine.try_resolve_visible_relation_kind(&reference)? else {
        return Ok(None);
    };
    resolved_regclass_oid(engine, &canonical, kind)
}

pub(crate) fn resolve_bound_regclass_oid(
    engine: &Engine,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    let Some((canonical, kind)) = engine.resolve_bound_relation_kind(name)?.into_found() else {
        return Ok(None);
    };
    resolved_regclass_oid(engine, &canonical, kind)
}

fn resolved_regclass_oid(
    engine: &Engine,
    canonical: &str,
    kind: &str,
) -> Result<Option<i64>, SQLError> {
    if kind == "sequence" {
        let object_id = engine
            .sequence_object_id(canonical)
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved sequence `{canonical}` has no object identity"
                ))
            })?;
        return Ok(Some(crate::sql::sequence_relation_oid(object_id)));
    }
    if kind == "index" {
        let relation =
            crate::RelationIdentity::from_legacy_name(canonical).map_err(SQLError::Internal)?;
        let catalog = engine.catalog_read_view();
        let mut resolution = engine.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::engine_capabilities::RelationLookupMode::Bound);
        let index = catalog_index_relations(&catalog, &resolution)?
            .into_iter()
            .find(|index| index.relation == relation)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved index `{canonical}` has no catalog relation"
                ))
            })?;
        return Ok(Some(index.oid()));
    }
    if kind == "table" {
        return super::super::table_relation_oid(engine, canonical)
            .map(Some)
            .map_err(|error| SQLError::Internal(error.to_string()));
    }
    let relation =
        crate::RelationIdentity::from_legacy_name(canonical).map_err(SQLError::Internal)?;
    match kind {
        "view" | "materialized view" => engine
            .durable
            .views
            .read()
            .get(&relation)
            .map(super::super::view_relation_oid)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved view `{canonical}` has no catalog definition"
                ))
            })
            .map(Some),
        "foreign table" => engine
            .durable
            .foreign_tables
            .read()
            .get(&relation)
            .map(super::super::foreign_table_relation_oid)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved foreign table `{canonical}` has no catalog definition"
                ))
            })
            .map(Some),
        other => Err(SQLError::Internal(format!(
            "unknown relation kind `{other}` for `{canonical}`"
        ))),
    }
}
