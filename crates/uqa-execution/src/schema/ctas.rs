//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE TABLE AS source execution, lock ordering, and physical row publication.
use crate::mutation::{constraints::context::ConstraintCatalog, publication::DocumentVectors};
use crate::query::CteScope;
use uqa_core::DocId;
use uqa_sql::schema::table_creation::create_table_as_columns;
use uqa_sql::{
    ast::{ColumnDef, ColumnType, OnCommitAction, RelationPersistence},
    plan::QueryPlan,
    routines::RoutineResolution,
    SQLError, SQLParam, SQLResult,
};
use uqa_storage::document_store::Document;

pub mod entry;

/// Optimize and execute at the call boundary so writer promotion cannot leave a stale query generation.
pub trait TableAsQuerySource {
    fn optimize(&self, plan: &QueryPlan) -> Result<QueryPlan, SQLError>;
    fn execute(&self, plan: &QueryPlan, params: &[SQLParam]) -> Result<SQLResult, SQLError>;
}
pub trait TableAsNamespace {
    fn ensure_temporary_privilege(&self) -> Result<(), SQLError>;
    fn temporary_target_name(&self, name: &str) -> Result<String, SQLError>;
    fn target_name(&self, name: &str) -> Result<String, SQLError>;
    fn relation_exists(&self, name: &str) -> Result<bool, SQLError>;
    fn ensure_create_privilege(&self, name: &str) -> Result<(), SQLError>;
    fn prepare_writer(&self) -> Result<bool, SQLError>;
}
pub trait TableAsPublication {
    fn create_relation(
        &self,
        name: &str,
        persistence: RelationPersistence,
        on_commit: OnCommitAction,
    ) -> Result<(), SQLError>;
    fn create_vector_field(
        &self,
        name: &str,
        column: &str,
        dimensions: u32,
    ) -> Result<bool, SQLError>;
    fn publish_columns(&self, name: &str, columns: &[ColumnDef]) -> Result<(), SQLError>;
    fn insert_document(
        &self,
        table: &str,
        id: DocId,
        document: Document,
        vectors: DocumentVectors,
    ) -> Result<(), SQLError>;
}
pub struct CreateTableAsContext<'a, S: Clone> {
    pub analysis_scope: &'a CteScope<S>,
    pub routines: &'a dyn RoutineResolution,
    pub queries: &'a dyn TableAsQuerySource,
    pub namespace: &'a dyn TableAsNamespace,
    pub publication: &'a dyn TableAsPublication,
    pub vectors: &'a dyn ConstraintCatalog,
}
pub struct CreateTableAsExecution<'a> {
    pub name: &'a str,
    pub if_not_exists: bool,
    pub column_names: &'a [String],
    pub with_no_data: bool,
    pub persistence: uqa_sql::ast::RelationPersistence,
    pub on_commit: uqa_sql::ast::OnCommitAction,
    pub query: &'a uqa_sql::plan::QueryPlan,
    pub params: &'a [SQLParam],
}

pub fn run_create_table_as<S: Clone>(
    context: &CreateTableAsContext<'_, S>,
    execution: &CreateTableAsExecution<'_>,
) -> Result<SQLResult, SQLError> {
    // PostgreSQL analyzes the CTAS source before target namespace resolution, collisions, or schema CREATE. Source execution still follows target validation, so an existing target wins over runtime expression errors and row locks.
    let temporary_privilege_error =
        if execution.persistence == uqa_sql::ast::RelationPersistence::Temporary {
            context.namespace.ensure_temporary_privilege().err()
        } else {
            None
        };
    let query_schema = crate::query::binding::analyze_query_plan_schema(
        context.routines,
        execution.query,
        execution.params,
        context.analysis_scope,
        None,
    )?;
    if let Some(error) = temporary_privilege_error {
        return Err(error);
    }
    let preliminary_name = create_table_as_target_name(context.namespace, execution)?;
    if should_skip_existing_create_table_as(
        context.namespace,
        &preliminary_name,
        execution.if_not_exists,
    )? {
        return Ok(SQLResult::empty().with_command_tag("CREATE TABLE AS"));
    }
    let columns = create_table_as_columns(&query_schema, execution.column_names)?;
    if execution.persistence != uqa_sql::ast::RelationPersistence::Temporary {
        context
            .namespace
            .ensure_create_privilege(&preliminary_name)?;
    }
    let executable = if execution.with_no_data {
        None
    } else {
        Some(context.queries.optimize(execution.query)?)
    };
    let executable = executable.as_ref().unwrap_or(execution.query);
    // A locking source must acquire and recheck every tuple before this session promotes its deferred backend transaction. Promoting first would invert the global writer and tuple-lock order against a concurrent updater. The target is checked again after promotion so a concurrent relation create still wins atomically.
    let locking_result =
        if !execution.with_no_data && crate::query::locking::query_has_row_locks(executable) {
            Some(context.queries.execute(executable, execution.params)?)
        } else {
            None
        };
    if execution.persistence != uqa_sql::ast::RelationPersistence::Temporary {
        context.namespace.prepare_writer()?;
    }
    let name = create_table_as_target_name(context.namespace, execution)?;
    if should_skip_existing_create_table_as(context.namespace, &name, execution.if_not_exists)? {
        return Ok(SQLResult::empty().with_command_tag("CREATE TABLE AS"));
    }
    if execution.persistence != uqa_sql::ast::RelationPersistence::Temporary {
        context.namespace.ensure_create_privilege(&name)?;
    }
    let result = if execution.with_no_data {
        None
    } else if let Some(result) = locking_result {
        Some(result)
    } else {
        Some(context.queries.execute(executable, execution.params)?)
    };
    if let Some(result) = &result {
        if result.columns.len() != columns.len() {
            return Err(SQLError::Internal(format!(
                "CREATE TABLE AS query schema width {} changed to {} during execution",
                columns.len(),
                result.columns.len()
            )));
        }
    }
    create_table_as_relation(
        context.publication,
        &name,
        &columns,
        execution.persistence,
        execution.on_commit,
    )?;
    let affected = result.as_ref().map_or(Ok(0), |result| {
        materialize_create_table_as_rows(
            context.publication,
            context.vectors,
            &name,
            &columns,
            result,
        )
    })?;
    let tag = if execution.with_no_data {
        "CREATE TABLE AS".to_string()
    } else {
        format!("SELECT {affected}")
    };
    Ok(SQLResult::from_affected(affected).with_command_tag(tag))
}

fn create_table_as_target_name(
    namespace: &dyn TableAsNamespace,
    execution: &CreateTableAsExecution<'_>,
) -> Result<String, SQLError> {
    if execution.persistence == uqa_sql::ast::RelationPersistence::Temporary {
        namespace.temporary_target_name(execution.name)
    } else {
        namespace.target_name(execution.name)
    }
}

fn should_skip_existing_create_table_as(
    namespace: &dyn TableAsNamespace,
    name: &str,
    if_not_exists: bool,
) -> Result<bool, SQLError> {
    if !namespace.relation_exists(name)? {
        return Ok(false);
    }
    if if_not_exists {
        return Ok(true);
    }
    Err(SQLError::Routine {
        sqlstate: "42P07".into(),
        message: format!("relation \"{name}\" already exists"),
    })
}

fn create_table_as_relation(
    publication: &dyn TableAsPublication,
    name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    persistence: uqa_sql::ast::RelationPersistence,
    on_commit: uqa_sql::ast::OnCommitAction,
) -> Result<(), SQLError> {
    publication.create_relation(name, persistence, on_commit)?;
    for column in columns {
        if let ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions) = column.ty {
            publication.create_vector_field(name, &column.name, dimensions)?;
        }
    }
    publication.publish_columns(name, columns)?;
    Ok(())
}

fn materialize_create_table_as_rows(
    publication: &dyn TableAsPublication,
    vectors: &dyn ConstraintCatalog,
    name: &str,
    columns: &[uqa_sql::ast::ColumnDef],
    result: &SQLResult,
) -> Result<u64, SQLError> {
    for (row_index, _) in result.rows.iter().enumerate() {
        let doc_id = u64::try_from(row_index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| SQLError::Internal("CREATE TABLE AS row count overflow".into()))?;
        let mut document = Document::new();
        for (column_index, column) in columns.iter().enumerate() {
            let value = result
                .value_at(row_index, column_index)
                .cloned()
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "CREATE TABLE AS row {row_index} is missing column {column_index}"
                    ))
                })?;
            document.insert(column.name.clone(), value);
        }
        let vectors = crate::mutation::vectors::document_vectors(vectors, name, &document)?;
        publication.insert_document(name, doc_id, document, vectors)?;
    }
    u64::try_from(result.rows.len())
        .map_err(|_| SQLError::Internal("CREATE TABLE AS row count overflow".into()))
}
