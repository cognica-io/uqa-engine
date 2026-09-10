//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical CREATE INDEX build ordering and catalog publication.
use uqa_sql::{
    ast::{CreateIndex, IndexKey},
    catalog::index::IndexDefinition,
    schema::indexes::{
        keys::require_column_key,
        names::{allocate_default_index_name, IndexNameCatalog},
        vectors::{resolve_vector_index_target, VectorIndexCatalog},
    },
    schema::SchemaExpressionCatalog,
    semantics::conflict::InferenceBindingScope,
    SQLError, SQLResult,
};
use uqa_storage::vector_index::{HNSWIndexParams, IVFIndexParams, VectorIndexSpec};
pub trait IndexCreationNamespace {
    fn resolve_index_table_name(&self, name: &str) -> Result<Option<String>, SQLError>;
    fn ensure_table_owner(&self, table: &str) -> Result<(), SQLError>;
    fn ensure_creation_privilege(&self, table: &str) -> Result<(), SQLError>;
    fn relation_exists(&self, name: &str) -> Result<bool, SQLError>;
}
pub trait IndexCreationPublication {
    fn add_text_field(
        &self,
        table: &str,
        column: &str,
        analyzer: Option<&str>,
    ) -> Result<(), SQLError>;
    fn rebuild_vector_field(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
        spec: VectorIndexSpec,
    ) -> Result<bool, SQLError>;
    fn register_index(
        &self,
        name: &str,
        method: &str,
        table: &str,
        keys: &[IndexKey],
        options: &[(String, String)],
        definition: &IndexDefinition,
    ) -> Result<(), SQLError>;
}
pub struct IndexCreationContext<'a> {
    pub namespace: &'a dyn IndexCreationNamespace,
    pub names: &'a dyn IndexNameCatalog,
    pub schema: &'a dyn SchemaExpressionCatalog,
    pub bindings: &'a dyn InferenceBindingScope,
    pub unique: super::IndexBuildContext<'a>,
    pub vectors: &'a dyn VectorIndexCatalog,
    pub publication: &'a dyn IndexCreationPublication,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}
pub fn run_create_index(
    context: &IndexCreationContext<'_>,
    mut c: CreateIndex,
) -> Result<SQLResult, SQLError> {
    c.table = context
        .namespace
        .resolve_index_table_name(&c.table)?
        .ok_or_else(|| SQLError::UnknownTable(c.table.clone()))?;
    context.namespace.ensure_table_owner(&c.table)?;
    context.namespace.ensure_creation_privilege(&c.table)?;
    let am = uqa_sql::schema::indexes::options::index_access_method(&c)?;

    let table_relation = uqa_core::RelationIdentity::from_legacy_name(&c.table)
        .map_err(|error| SQLError::Internal(format!("resolve index table: {error}")))?;
    let name = if let Some(name) = c.name.as_ref() {
        name.clone()
    } else {
        allocate_default_index_name(context.names, &table_relation, &c.columns)?
    };
    let relation = uqa_core::RelationIdentity::new(&table_relation.schema, &name);
    if context
        .namespace
        .relation_exists(&relation.qualified_name())?
    {
        if c.if_not_exists {
            context.notices.lock().push((
                "NOTICE".into(),
                format!("relation \"{name}\" already exists, skipping"),
            ));
            return Ok(SQLResult::empty());
        }
        return Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{name}\" already exists"),
        });
    }

    let definition = uqa_sql::schema::indexes::keys::prepare_index_definition(
        context.schema,
        context.bindings,
        &mut c,
    )?;
    super::validate_unique_index(&context.unique, &c, &name)?;

    match am.as_str() {
        "gin" => {
            for col in &c.columns {
                let analyzer = c
                    .options
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                let column = require_column_key(col, "gin")?;
                context
                    .publication
                    .add_text_field(&c.table, column, analyzer)?;
            }
        }
        "" | "btree" => {}
        "ivf" | "hnsw" => create_vector_index(context.vectors, context.publication, &c, &am)?,
        _ => unreachable!("access method was validated above"),
    }
    // Publish the original option values and bound key metadata so reopening restores the same physical index.
    let catalog_index_type = if am.is_empty() { "btree" } else { &am };
    context.publication.register_index(
        &relation.qualified_name(),
        catalog_index_type,
        &c.table,
        &c.columns,
        &c.options,
        &definition,
    )?;
    Ok(SQLResult::empty())
}

fn create_vector_index(
    catalog: &dyn VectorIndexCatalog,
    publication: &dyn IndexCreationPublication,
    statement: &CreateIndex,
    access_method: &str,
) -> Result<(), SQLError> {
    let spec = vector_index_spec(access_method, &statement.options)?;
    let target = resolve_vector_index_target(catalog, statement, access_method)?;
    for (column, dimensions) in target.fields {
        if !publication.rebuild_vector_field(&target.table, column, dimensions, spec)? {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING {access_method}: relation `{}` does not exist",
                target.table
            )));
        }
    }
    Ok(())
}
fn vector_index_spec(
    access_method: &str,
    options: &[(String, String)],
) -> Result<VectorIndexSpec, SQLError> {
    use uqa_sql::schema::indexes::options::{parse_hnsw_index_options, parse_ivf_index_options};
    match access_method {
        "ivf" => {
            let parsed = parse_ivf_index_options(options)?;
            let defaults = IVFIndexParams::default();
            Ok(VectorIndexSpec::IVF(IVFIndexParams {
                nlist: parsed.nlist.unwrap_or(defaults.nlist),
                nprobe: parsed.nprobe.unwrap_or(defaults.nprobe),
                train_threshold: parsed.train_threshold.unwrap_or(defaults.train_threshold),
            }))
        }
        "hnsw" => {
            let parsed = parse_hnsw_index_options(options)?;
            let defaults = HNSWIndexParams::default();
            let params = HNSWIndexParams {
                m: parsed.m.unwrap_or(defaults.m),
                ef_construction: parsed.ef_construction.unwrap_or(defaults.ef_construction),
                ef_search: parsed.ef_search.unwrap_or(defaults.ef_search),
                rebuild_threshold: parsed
                    .rebuild_threshold
                    .unwrap_or(defaults.rebuild_threshold),
                seed: parsed.seed.unwrap_or(defaults.seed),
            };
            params
                .validate()
                .map(VectorIndexSpec::HNSW)
                .map_err(|error| {
                    SQLError::TypeMismatch(format!("CREATE INDEX USING hnsw: {error}"))
                })
        }
        _ => unreachable!("vector access method was validated above"),
    }
}
