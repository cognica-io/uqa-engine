//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation OID resolution for SQL names and already-bound dependency identities.

use super::{
    catalog_index_relations, parse_dash_or_oid, qualified_name, qualified_name_list, relation_name,
    CatalogContext, SQLError,
};

/// `regclassin`: `-` and a string of digits are OIDs, and any other string is a relation name, possibly schema-qualified, that the search path resolves. A name `SplitIdentifierString` rejects is `42602`, and a relation the lookup does not find, whether its schema or the relation is missing, is `42P01` naming the parsed components as `NameListToString` joins them.
pub fn regclass_input_oid(context: &CatalogContext<'_>, name: &str) -> Result<i64, SQLError> {
    if let Some(oid) = parse_dash_or_oid(name)? {
        return Ok(oid);
    }
    let names = uqa_sql::parse_regobject_name(name).ok_or_else(|| SQLError::Routine {
        sqlstate: "42602".into(),
        message: "invalid name syntax".into(),
    })?;
    let (schema, local) = relation_name(&names)?;
    let reference = schema.map_or_else(
        || uqa_sql::expr::quote_ident(local),
        |schema| qualified_name(schema, local),
    );
    let Some((canonical, kind)) = context.try_resolve_visible_relation_kind(&reference)? else {
        return Err(SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!(
                "relation \"{}\" does not exist",
                qualified_name_list(&names)
            ),
        });
    };
    resolved_regclass_oid(context, &canonical, kind)
}

/// `to_regclass`: [`regclass_input_oid`] with the errors `regclassin` reports through its error context, a malformed number or name and a relation it does not find, as no relation.
pub fn lookup_regclass_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    match regclass_input_oid(context, name) {
        Ok(oid) => Ok(Some(oid)),
        Err(SQLError::Routine { sqlstate, .. })
            if matches!(sqlstate.as_str(), "22P02" | "22003" | "42602" | "42P01") =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn resolve_bound_regclass_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    let Some((canonical, kind)) = context.resolve_bound_relation_kind(name)?.into_found() else {
        return Ok(None);
    };
    resolved_regclass_oid(context, &canonical, kind).map(Some)
}

fn resolved_regclass_oid(
    context: &CatalogContext<'_>,
    canonical: &str,
    kind: &str,
) -> Result<i64, SQLError> {
    if let Some(relation) = uqa_sql::catalog::SystemRelation::from_qualified_name(canonical) {
        return Ok(relation.oid());
    }
    if kind == "sequence" {
        let object_id = context
            .sequence_object_id(canonical)
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved sequence `{canonical}` has no object identity"
                ))
            })?;
        return Ok(context.catalog_read_view().sequence_catalog_oid(&object_id));
    }
    if kind == "index" {
        let relation =
            uqa_core::RelationIdentity::from_legacy_name(canonical).map_err(SQLError::Internal)?;
        let catalog = context.catalog_read_view();
        let mut resolution = context.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
        let index = catalog_index_relations(&catalog, &resolution)?
            .into_iter()
            .find(|index| index.relation == relation)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved index `{canonical}` has no catalog relation"
                ))
            })?;
        return Ok(index.oid());
    }
    if kind == "table" {
        return super::super::table_relation_oid(context, canonical)
            .map_err(|error| SQLError::Internal(error.to_string()));
    }
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(canonical).map_err(SQLError::Internal)?;
    match kind {
        "view" | "materialized view" => context
            .catalog_read_view()
            .snapshot()
            .definitions
            .views
            .get(&relation)
            .map(super::super::view_relation_oid)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved view `{canonical}` has no catalog definition"
                ))
            }),
        "foreign table" => context
            .catalog_read_view()
            .snapshot()
            .definitions
            .foreign_tables
            .get(&relation)
            .map(super::super::foreign_table_relation_oid)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved foreign table `{canonical}` has no catalog definition"
                ))
            }),
        "composite type" => context
            .catalog_read_view()
            .snapshot()
            .definitions
            .composites
            .get(canonical)
            .map(|definition| i64::from(definition.relation_oid))
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved composite type `{canonical}` has no catalog definition"
                ))
            }),
        other => Err(SQLError::Internal(format!(
            "unknown relation kind `{other}` for `{canonical}`"
        ))),
    }
}
