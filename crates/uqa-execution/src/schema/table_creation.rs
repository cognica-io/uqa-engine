//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute table creation with the original namespace, sequence, and schema write boundaries.
use super::{
    publication::{self, SchemaWriteTransaction},
    sequences::{
        implicit::{self, ImplicitSequenceContext},
        ownership::{self, ImplicitOwnershipContext},
    },
};
use crate::catalog::identity::{allocate_catalog_object_id, CatalogIdentityReservationContext};
use uqa_sql::ast::{
    ColumnType, CreateTable, DeferredCreateTable, OnCommitAction, RelationPersistence,
    TableConstraintSet, TableHierarchy,
};
use uqa_sql::catalog::relation_oids::{RelationCatalogOids, RelationOidKind};
use uqa_sql::schema::constraint_metadata::CatalogIdentityAllocator;
use uqa_sql::schema::table_creation::declaration::{self, CreateTableAnalysisContext};
use uqa_sql::schema::table_creation::{checks, not_nulls};
use uqa_sql::{SQLError, SQLResult};
use uqa_storage::{StorageBackendError, StorageBackendResult};

pub mod entry;

pub trait TableCreationNamespace {
    fn prepare_writer(&self) -> Result<bool, SQLError>;
    fn relation_exists(&self, name: &str) -> Result<bool, SQLError>;
}
pub trait TableCreationPublication {
    /// Create the relation under the OIDs `CREATE TABLE` allocated for it where `heap_create_with_catalog` allocates them.
    fn create_table(
        &self,
        name: &str,
        persistence: RelationPersistence,
        on_commit: OnCommitAction,
        owner: &crate::catalog::security::roles::locking::RoleBinding,
        catalog_oids: RelationCatalogOids,
    ) -> StorageBackendResult<()>;
    fn create_vector_field(
        &self,
        table: &str,
        field: String,
        dimensions: u32,
    ) -> StorageBackendResult<bool>;
    fn install_hierarchy(&self, table: &str, hierarchy: TableHierarchy)
        -> StorageBackendResult<()>;
    fn persist_schema(&self, table: &str) -> StorageBackendResult<bool>;
    fn refresh_value_indexes(&self, table: &str) -> StorageBackendResult<()>;
    /// Reject a new partition whose bound accepts a row already stored in the parent's default partition.
    fn validate_default_partition_rows(
        &self,
        parent: &str,
        bound: &uqa_sql::ast::PartitionBound,
    ) -> Result<(), SQLError>;
}
pub struct CreateTableContext<'a> {
    pub creation: crate::schema::namespaces::relations::RelationCreationContext<'a>,
    pub namespace: &'a dyn TableCreationNamespace,
    pub analysis: CreateTableAnalysisContext<'a>,
    pub sequences: ImplicitSequenceContext<'a>,
    pub ownership: ImplicitOwnershipContext<'a>,
    pub schema_transactions: &'a dyn SchemaWriteTransaction,
    pub publication: &'a dyn TableCreationPublication,
    /// The database's OID counter and catalog, from which the statement draws the OIDs of the relation and of each object it defines.
    pub identities: CatalogIdentityReservationContext<'a>,
    pub notices: &'a crate::query::NoticeQueue,
}
fn storage_error(action: &str, error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}

pub fn run_create_table(
    context: &CreateTableContext<'_>,
    mut table: CreateTable,
) -> Result<SQLResult, SQLError> {
    let owner = context.creation.bind_owner()?;
    let Some((name, persistence)) = preflight(
        context,
        &table.name,
        table.persistence,
        table.if_not_exists,
        ExistingRelation::Deferred,
    )?
    else {
        return Ok(SQLResult::empty());
    };
    table.name = name;
    table.persistence = persistence;
    declaration::transform_create_table(&context.analysis, &mut table)?;
    create_after_preflight(context, table, &owner)
}
pub fn run_create_table_if_not_exists(
    context: &CreateTableContext<'_>,
    deferred: DeferredCreateTable,
) -> Result<SQLResult, SQLError> {
    let owner = context.creation.bind_owner()?;
    let Some((name, persistence)) = preflight(
        context,
        &deferred.name,
        deferred.persistence,
        true,
        ExistingRelation::Deferred,
    )?
    else {
        return Ok(SQLResult::empty());
    };
    let mut table = uqa_sql::resolve_deferred_create_table(&deferred)?;
    table.name = name;
    table.persistence = persistence;
    declaration::transform_create_table(&context.analysis, &mut table)?;
    create_after_preflight(context, table, &owner)
}

/// When a relation that already has the name is an error. `transformCreateStmt` skips `IF NOT EXISTS` before it analyzes the declaration, but `heap_create_with_catalog` reports the collision only after `MergeAttributes`, `BuildDescForRelation` and `CheckAttributeNamesTypes` accepted the row type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExistingRelation {
    Deferred,
    Reported,
}
/// Resolve the new table's name and persistence as `RangeVarGetAndCheckCreationNamespace` does, before `transformCreateStmt` looks for a relation that already has the name.
fn preflight(
    context: &CreateTableContext<'_>,
    name: &str,
    persistence: RelationPersistence,
    if_not_exists: bool,
    existing: ExistingRelation,
) -> Result<Option<(String, RelationPersistence)>, SQLError> {
    if !context
        .creation
        .targets_temporary_namespace(name, persistence)?
    {
        context.namespace.prepare_writer()?;
    }
    let (name, persistence) = context.creation.relation_target(name, persistence)?;
    if context.namespace.relation_exists(&name)? {
        let local = uqa_core::RelationIdentity::from_legacy_name(&name)
            .map_err(SQLError::Internal)?
            .name;
        if if_not_exists {
            context.notices.push(
                uqa_sql::SQLNotice::notice(format!(
                    "relation \"{local}\" already exists, skipping"
                ))
                .with_sqlstate("42P07"),
            );
            return Ok(None);
        }
        if existing == ExistingRelation::Reported {
            return Err(SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{local}\" already exists"),
            });
        }
    }
    Ok(Some((name, persistence)))
}
/// Create the table in `DefineRelation`'s order, after the sequences of its columns: `MergeAttributes` and the relation's name, then the relation's OIDs as `heap_create_with_catalog` allocates them, its expressions and constraints, each taking its OIDs as it is defined, and then the relation and its catalog state. A statement that fails after the relation's OIDs has used them, as the counter advances outside the transaction.
fn create_after_preflight(
    context: &CreateTableContext<'_>,
    mut table: CreateTable,
    owner: &crate::catalog::security::roles::locking::RoleBinding,
) -> Result<SQLResult, SQLError> {
    implicit::materialize_implicit_sequences(
        &context.sequences,
        "CREATE TABLE",
        &table.name,
        &mut table.columns,
        table.persistence,
    )?;
    let mut notices = Vec::new();
    let inherited =
        declaration::prepare_create_table_declaration(&context.analysis, &mut table, &mut notices);
    // A notice reaches the client before the error that ends the statement.
    for notice in notices {
        context.notices.push(notice);
    }
    let inherited = inherited?;
    context.creation.retain_owner(owner)?;
    if preflight(
        context,
        &table.name,
        table.persistence,
        table.if_not_exists,
        ExistingRelation::Reported,
    )?
    .is_none()
    {
        return Ok(SQLResult::empty());
    }
    // `heap_create_with_catalog` checks the row type's name, allocates the relation's OIDs and stores the inherited expressions before `DefineRelation` analyzes the statement's own.
    context.creation.reserve_row_type_name(&table.name)?;
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(&table.name).map_err(SQLError::Internal)?;
    let mut allocator = context.identities.allocator(allocate_catalog_object_id);
    let catalog_oids = allocator.allocate_relation_oids(RelationOidKind::Table, &relation)?;
    define_expressions_and_constraints(
        context,
        &mut table,
        &inherited,
        catalog_oids.relation,
        &mut allocator,
    )?;
    let mut vector_fields = Vec::new();
    for column in &table.columns {
        match &column.ty {
            ColumnType::Vector(dim) | ColumnType::Tensor(dim) => {
                vector_fields.push((column.name.clone(), *dim));
            }
            _ => {}
        }
    }
    context
        .publication
        .create_table(
            &table.name,
            table.persistence,
            table.on_commit,
            owner,
            catalog_oids,
        )
        .map_err(|error| storage_error("CREATE TABLE", error))?;
    for (field, dimensions) in vector_fields {
        context
            .publication
            .create_vector_field(&table.name, field, dimensions)
            .map_err(|error| storage_error("CREATE TABLE vector field", error))?;
    }
    uqa_sql::schema::table_creation::foreign_keys::define_foreign_keys(
        &context.analysis,
        &mut table,
        inherited.foreign_keys,
        &mut allocator,
    )?;
    publish_catalog_state(context, &table, table.columns.clone())?;
    Ok(SQLResult::empty())
}

/// Publish the created relation's columns and constraints, the ownership of its sequences and its hierarchy, then persist its schema and refresh its indexes.
fn publish_catalog_state(
    context: &CreateTableContext<'_>,
    table: &CreateTable,
    registered_columns: Vec<uqa_sql::ast::ColumnDef>,
) -> Result<(), SQLError> {
    let constraints = TableConstraintSet {
        columns_declared: Some(true),
        persistence: table.persistence,
        on_commit: table.on_commit,
        checks: table.checks.clone(),
        foreign_keys: table.foreign_keys.clone(),
        key_constraints: table.key_constraints.clone(),
        hierarchy: table.hierarchy.clone(),
        // The created relation's state carries its OIDs; publication records them.
        catalog_oids: None,
        row_type_array_name: None,
    };
    context
        .schema_transactions
        .with_schema_write(Box::new(|schema| {
            publication::replace_constraint_state(
                schema,
                &table.name,
                registered_columns,
                constraints,
            )
        }))
        .map_err(|error| storage_error("CREATE TABLE constraints", error))?;
    ownership::attach_table_owners(&context.ownership, &table.name)?;
    context
        .publication
        .install_hierarchy(&table.name, table.hierarchy.clone())
        .map_err(|error| storage_error("CREATE TABLE hierarchy", error))?;
    republish_ancestor_references(context, table)?;
    context
        .publication
        .persist_schema(&table.name)
        .map_err(|error| storage_error("CREATE TABLE", error))?;
    context
        .publication
        .refresh_value_indexes(&table.name)
        .map_err(|error| storage_error("CREATE TABLE btree indexes", error))
}

/// The inherited expressions, the defaults and generation expressions in column order, the partition bound and key, the keys a partition clones, the CHECK constraints in written order, the NOT NULL constraints as `AddRelationNotNullConstraints` creates them, and then the declared keys, in the order `DefineRelation` and the commands it queues define them; each defined object takes its OIDs from `allocate` as it is defined. `relation_oid` is the relation's OID, which a constraint name violation reports.
fn define_expressions_and_constraints(
    context: &CreateTableContext<'_>,
    table: &mut CreateTable,
    inherited: &declaration::InheritedDefinitions,
    relation_oid: u32,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    declaration::define_inherited_expressions(table, inherited, allocate)?;
    declaration::define_create_table_defaults(&context.analysis, table, allocate)?;
    bind_partitioning(context, table)?;
    let indexes =
        declaration::clone_create_table_parent_keys(&context.analysis, table, inherited, allocate)?;
    let cloned = declaration::cloned_constraint_names(table, inherited);
    let mut notices = Vec::new();
    let checks = checks::define_create_table_checks(
        &context.analysis,
        table,
        &cloned,
        &mut notices,
        allocate,
    );
    // A notice reaches the client before the error that ends the statement.
    for notice in notices {
        context.notices.push(notice);
    }
    checks?;
    not_nulls::define_not_null_constraints(
        &context.analysis,
        table,
        inherited,
        relation_oid,
        allocate,
    )?;
    declaration::define_create_table_constraints(table, indexes, inherited, allocate)
}

/// `DefineRelation` binds a new partition's bound and the table's partition key once the relation exists, and `check_default_partition_contents` rejects a bound that accepts a row the parent's default partition holds.
fn bind_partitioning(
    context: &CreateTableContext<'_>,
    table: &mut CreateTable,
) -> Result<(), SQLError> {
    declaration::bind_create_table_partitioning(&context.analysis, table)?;
    if let (Some(parent), Some(bound)) = (
        table.hierarchy.parents.first(),
        table.hierarchy.partition_bound.as_ref(),
    ) {
        context
            .publication
            .validate_default_partition_rows(parent, bound)?;
    }
    Ok(())
}

/// The foreign keys referencing a new partition's ancestors derive constraints on it.
fn republish_ancestor_references(
    context: &CreateTableContext<'_>,
    table: &CreateTable,
) -> Result<(), SQLError> {
    let Some(parent) = table
        .hierarchy
        .parents
        .first()
        .filter(|_| table.hierarchy.is_partition())
    else {
        return Ok(());
    };
    let parent = parent.clone();
    context
        .schema_transactions
        .with_schema_write(Box::new(move |schema| {
            publication::referenced_partitions::republish_referencing_tables(
                schema,
                &parent,
                crate::row_locks::RelationLockMode::ShareRowExclusive,
            )
            .map_err(|error| {
                StorageBackendError::backend("CREATE TABLE derived constraints", error)
            })
        }))
        .map_err(|error| storage_error("CREATE TABLE derived constraints", error))
}
