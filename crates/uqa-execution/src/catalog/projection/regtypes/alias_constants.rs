//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog type and routine names printed from a catalog view and the session's name resolution, without a catalog context.

use std::sync::Arc;

use uqa_sql::ast::ColumnType;
use uqa_sql::SQLError;

use crate::catalog::{CatalogReadView, RelationNameResolution};

use super::{
    collations, format_regproc, format_regprocedure, format_regtype, format_type, namespace_name,
    procedures, OutputVisibility, RegtypeOutputCatalog,
};

/// The output of `regtype`, `regproc`, `regprocedure` and `regnamespace` constants in reconstructed SQL, built from a catalog view and the session's name resolution when a deparser first prints one.
pub(crate) struct AliasConstantOutput {
    pub(super) catalog: Arc<RegtypeOutputCatalog>,
    pub(super) visibility: OutputVisibility,
}

impl AliasConstantOutput {
    /// Type spelling reuses `format_type`'s modifiers and the same visible catalog identity as OID-alias output.
    pub(crate) fn type_name(&self, ty: &ColumnType) -> Result<String, SQLError> {
        let oid = uqa_sql::catalog::type_metadata::pg_type_oid(ty);
        if oid == 0 {
            return Ok(ty.sql_name());
        }
        format_type::format_catalog_type(
            &self.catalog,
            &self.visibility,
            oid,
            Some(uqa_sql::catalog::type_metadata::pg_type_modifier(ty)),
        )
    }

    pub(crate) fn build(
        catalog: &CatalogReadView,
        resolution: &RelationNameResolution,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            catalog: RegtypeOutputCatalog::without_relations(catalog, resolution)?,
            visibility: OutputVisibility::from_resolution(catalog, resolution),
        })
    }

    /// Names a selected catalog signature using the same visibility rule as `regprocedure` output.
    pub(crate) fn routine_name_parts(
        &self,
        schema: &str,
        name: &str,
        argument_types: &[i64],
    ) -> Option<Vec<String>> {
        procedures::signature_name_parts(
            &self.visibility,
            &self.catalog,
            schema,
            name,
            argument_types,
        )
    }

    /// The constant's output text, or `None` when no object holds the OID or the type is not one this output prints.
    pub(crate) fn text(&self, ty: &ColumnType, oid: i64) -> Option<String> {
        match ty {
            ColumnType::Regtype => format_regtype(&self.visibility, &self.catalog, oid),
            ColumnType::Regcollation => collations::format_regcollation(&self.visibility, oid),
            ColumnType::Regproc => format_regproc(&self.visibility, &self.catalog, oid),
            ColumnType::Regprocedure => format_regprocedure(&self.visibility, &self.catalog, oid),
            ColumnType::Regnamespace => {
                namespace_name(&self.catalog, oid).map(uqa_sql::expr::quote_ident)
            }
            _ => None,
        }
    }

    /// The `data_type`, `type_udt_schema` and `type_udt_name` that `information_schema.routines` derives from a routine's `prorettype` through `pg_type` and `pg_namespace`: `ARRAY` for a varlena type with an element type, `format_type(oid, NULL)` for another `pg_catalog` type, and `USER-DEFINED` otherwise. `None` when no type has the OID, as the view's outer join leaves all three NULL.
    pub(crate) fn routine_result_type(
        &self,
        oid: i64,
    ) -> Result<Option<(String, &str, &str)>, SQLError> {
        let Some(entry) = self.catalog.types.get(&oid) else {
            return Ok(None);
        };
        let Some(schema) = namespace_name(&self.catalog, entry.namespace_oid) else {
            return Ok(None);
        };
        let data_type = if entry.element_oid != 0 && entry.length == -1 {
            "ARRAY".to_owned()
        } else if schema == "pg_catalog" {
            format_type::format_catalog_type(&self.catalog, &self.visibility, oid, None)?
        } else {
            "USER-DEFINED".to_owned()
        };
        Ok(Some((data_type, schema, entry.name.as_str())))
    }
}
