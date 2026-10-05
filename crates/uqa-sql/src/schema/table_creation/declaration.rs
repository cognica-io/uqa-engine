//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind complete table declarations around their sequence and catalog publication boundaries.
use crate::ast::{ColumnDef, CreateTable, Expr, ForeignKey};
use crate::schema::constraint_metadata::{
    identity::{materialize_default_oid, materialize_not_null_identity},
    materialize_check_identity, materialize_foreign_key_identity, CatalogIdentityAllocator,
    ConstraintMetadataError,
};
use crate::schema::constraints::validate_foreign_key_definition;
use crate::schema::foreign_keys::{resolve_foreign_key_parent, ForeignKeyDefinitionContext};
use crate::schema::indexes::names::{ConstraintIndexNamer, IndexNameCatalog};
use crate::schema::inheritance::InheritanceContext;
pub use crate::schema::table_creation::keys::InheritedDefinitions;
use crate::schema::{SchemaBindingContext, SchemaExpressionCatalog};
use crate::semantics::conflict::InferenceBindingScope;
use crate::type_resolution::FunctionTypeResolver;
use crate::SQLError;
use std::collections::BTreeSet;

pub struct CreateTableAnalysisContext<'a> {
    pub types: &'a dyn FunctionTypeResolver,
    pub schema: &'a dyn SchemaExpressionCatalog,
    pub bindings: &'a dyn InferenceBindingScope,
    pub inheritance: InheritanceContext<'a>,
    pub index_names: &'a dyn IndexNameCatalog,
    pub foreign_keys: ForeignKeyDefinitionContext<'a>,
}

/// Examine the statement's elements in written order as `transformCreateStmt` does while it analyzes the statement, before the relation's name is checked: each column's type and then its clauses, and each NOT NULL table constraint; then validate the declared keys.
pub fn transform_create_table(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
) -> Result<(), SQLError> {
    use crate::ast::DeclaredElement;
    let target = super::column_declarations::ColumnDeclarationTarget {
        table: &c.qualifier,
        partitioned: c.hierarchy.partition_spec.is_some(),
        partition: c.hierarchy.partition_bound.is_some(),
    };
    let lost =
        || SQLError::Internal("the written order of CREATE TABLE elements lost a column".into());
    let mut columns = c.columns.iter_mut();
    let mut deferrable_key = false;
    for element in &c.element_order {
        match element {
            DeclaredElement::Column(declaration) => {
                let column = columns.next().ok_or_else(lost)?;
                super::column_declarations::check_serial_array(declaration)?;
                if !c.untyped_columns.contains(&column.name) {
                    column.ty = crate::type_resolution::resolve_declared_column_type(
                        context.types,
                        &column.ty,
                    )?;
                }
                deferrable_key |= super::column_declarations::check_column_declaration(
                    declaration,
                    &column.name,
                    target,
                )?;
            }
            DeclaredElement::NotNull { no_inherit } => {
                if target.partitioned && *no_inherit {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "not-null constraints on partitioned tables cannot be NO INHERIT"
                            .into(),
                    });
                }
            }
            DeclaredElement::DeferrableKey => deferrable_key = true,
        }
    }
    if columns.next().is_some() {
        return Err(lost());
    }
    if deferrable_key {
        return Err(SQLError::Unsupported(
            "CREATE TABLE: DEFERRABLE PRIMARY KEY and UNIQUE constraints are not supported".into(),
        ));
    }
    c.element_order.clear();
    super::keys::transform_declared_keys(&context.inheritance, c)
}

/// Describe the new table's columns before its name is claimed: the parents' columns merge ahead of the local ones as `MergeAttributes` merges them, the declared primary key's columns take their NOT NULL constraints, `BuildDescForRelation` requires `USAGE` on every column's type, and `CheckAttributeNamesTypes` rejects system column names and pseudo-types.
pub fn prepare_create_table_declaration(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
    notices: &mut Vec<crate::SQLNotice>,
) -> Result<InheritedDefinitions, SQLError> {
    let declared = c.key_constraints.len();
    let declared_foreign_keys = c.foreign_keys.len();
    let expressions =
        super::super::inheritance::merge_create_table_hierarchy(&context.inheritance, c, notices)?;
    let keys = c.key_constraints.len() - declared;
    let foreign_keys = c.foreign_keys.len() - declared_foreign_keys;
    super::keys::declare_primary_key_not_null(&mut c.columns, &c.key_constraints[keys..]);
    for column in &c.columns {
        context.types.require_type_usage(&column.ty)?;
    }
    super::validate_create_table_columns(c)?;
    let unique_indexes = match c.hierarchy.parents.first() {
        Some(parent) if c.hierarchy.is_partition() && c.hierarchy.partition_spec.is_some() => {
            context
                .inheritance
                .catalog
                .unique_index_keys(parent)
                .map_err(|error| SQLError::Internal(format!("read parent indexes: {error}")))?
        }
        _ => Vec::new(),
    };
    Ok(InheritedDefinitions {
        expressions,
        keys,
        foreign_keys,
        unique_indexes,
    })
}

/// `heap_create_with_catalog` stores what the parents give with the relation, before the statement's own expressions are analyzed: the inherited defaults and generation expressions in column order, then the inherited CHECK constraints in the order `MergeAttributes` collected them.
pub fn define_inherited_expressions(
    c: &mut CreateTable,
    inherited: &InheritedDefinitions,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    for column in &mut c.columns {
        if inherited.expressions.contains(&column.name) {
            allocate_expression_identity(column, allocate)?;
        }
    }
    for check in c.checks.iter_mut().filter(|check| !check.is_local) {
        materialize_check_identity(&mut check.object_id, &mut check.catalog_oid, allocate)
            .map_err(ConstraintMetadataError::into_sql_error)?;
    }
    Ok(())
}

/// `StoreAttrDefault`: a column's default or generation expression takes its `pg_attrdef` OID when it is stored; a column without one takes none. The column's own incarnation comes first.
fn allocate_expression_identity(
    column: &mut ColumnDef,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    if column.object_id.is_none() {
        column.object_id = Some(
            allocate
                .allocate_object_id("column")
                .map_err(ConstraintMetadataError::into_sql_error)?,
        );
    }
    materialize_default_oid(column, allocate).map_err(ConstraintMetadataError::into_sql_error)?;
    Ok(())
}

/// `AddRelationNotNullConstraints` stores the NOT NULL constraints after the CHECKs: the ones the statement declares, then the ones only parents give.
pub fn define_not_null_identities(
    c: &mut CreateTable,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    for local in [true, false] {
        for column in c
            .columns
            .iter_mut()
            .filter(|column| column.not_null && column.not_null_is_local == local)
        {
            materialize_not_null_identity(column, allocate)
                .map_err(ConstraintMetadataError::into_sql_error)?;
        }
    }
    Ok(())
}

/// `ATAddForeignKeyConstraint` creates each foreign key's constraint row once the key it references is found: the column foreign keys, then the table's, after the ones a partition cloned.
pub fn define_foreign_key_identities(
    columns: &mut [ColumnDef],
    foreign_keys: &mut [ForeignKey],
    cloned: usize,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    for reference in columns
        .iter_mut()
        .filter_map(|column| column.references.as_mut())
    {
        materialize_foreign_key_identity(
            &mut reference.object_id,
            &mut reference.catalog_identity,
            allocate,
        )
        .map_err(ConstraintMetadataError::into_sql_error)?;
    }
    for foreign_key in &mut foreign_keys[cloned..] {
        materialize_foreign_key_identity(
            &mut foreign_key.object_id,
            &mut foreign_key.catalog_identity,
            allocate,
        )
        .map_err(ConstraintMetadataError::into_sql_error)?;
    }
    Ok(())
}

/// Bind the partition bound and key of the created relation, which `DefineRelation` computes once the relation exists.
pub fn bind_create_table_partitioning(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
) -> Result<(), SQLError> {
    super::super::inheritance::bind_create_table_partitioning(&context.inheritance, c)
}

/// The defaults and generation expressions of the new table's columns, which `DefineRelation` transforms in column order before it binds the table's partitioning, since a partition key may name a generated column; each stored expression takes its `pg_attrdef` OID as `AddRelationNewConstraints` stores it.
pub fn define_create_table_defaults(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    let binding = context.bindings.binding_scope()?;
    let schema = SchemaBindingContext {
        catalog: context.schema,
        binding: &binding.context(),
    };
    let snapshot = c.columns.clone();
    for index in 0..c.columns.len() {
        let column = &mut c.columns[index];
        if let Some(default) = &mut column.default {
            if !super::super::defaults::validate_default_expression(
                &schema,
                default,
                &column.ty,
                &column.name,
            )? {
                column.default = None;
            }
        }
        super::super::generated::prepare_generated_column(
            &schema,
            &c.qualifier,
            &snapshot,
            &mut c.columns,
            index,
            &c.foreign_keys,
        )?;
        allocate_expression_identity(&mut c.columns[index], allocate)?;
    }
    Ok(())
}

/// The keys a partition clones from its parent, which `DefineRelation` creates once the table's own partition key is stored and before its CHECK constraints: each is checked against that partition key and its index named, beside the CHECKs the table inherits. The namer then names the declared keys.
pub fn clone_create_table_parent_keys<'a>(
    context: &CreateTableAnalysisContext<'a>,
    c: &mut CreateTable,
    inherited: &InheritedDefinitions,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<ConstraintIndexNamer<'a>, SQLError> {
    super::keys::clone_parent_keys(context.index_names, c, inherited, allocate)
}

/// The names of the constraints a new table holds before its CHECKs besides the ones it inherits: the keys and foreign keys a partition clones from its parent.
pub fn cloned_constraint_names(
    c: &CreateTable,
    inherited: &InheritedDefinitions,
) -> BTreeSet<String> {
    c.key_constraints[..inherited.keys]
        .iter()
        .filter_map(|key| key.name.clone())
        .chain(
            c.foreign_keys[..inherited.foreign_keys]
                .iter()
                .filter_map(|foreign_key| foreign_key.name.clone()),
        )
        .collect()
}

/// Define the declared keys and then bind the foreign keys, which `PostgreSQL` creates after the keys so that a foreign key can reference a key of its own table.
pub fn define_create_table_constraints(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
    indexes: ConstraintIndexNamer<'_>,
    inherited: &InheritedDefinitions,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    super::keys::define_declared_keys(indexes, c, inherited, allocate)?;
    bind_create_table_relation_references(context.foreign_keys.catalog, c, inherited)?;
    for foreign_key in &mut c.foreign_keys {
        if !foreign_key.period {
            continue;
        }
        if foreign_key.ref_table == c.name {
            validate_foreign_key_definition(
                &c.name,
                &c.columns,
                &c.name,
                &c.columns,
                &c.key_constraints,
                foreign_key,
            )?;
        } else {
            let (canonical, parent_columns, parent_keys) =
                resolve_foreign_key_parent(&context.foreign_keys, &foreign_key.ref_table)?;
            validate_foreign_key_definition(
                &c.name,
                &c.columns,
                &canonical,
                &parent_columns,
                &parent_keys,
                foreign_key,
            )?;
            foreign_key.ref_table = canonical;
        }
    }
    Ok(())
}

pub fn bind_created_table_foreign_keys(
    context: &ForeignKeyDefinitionContext<'_>,
    c: &mut CreateTable,
    registered_columns: &mut [ColumnDef],
) -> Result<(), SQLError> {
    let local_columns = registered_columns.to_vec();
    for column in registered_columns {
        let Some(reference) = column.references.clone() else {
            continue;
        };
        let mut foreign_key = super::super::foreign_keys::column_foreign_key(column, &reference);
        super::super::foreign_keys::validate_bound_foreign_key_definition_with_local_state(
            context,
            &c.name,
            Some(&local_columns),
            Some(&c.key_constraints),
            &mut foreign_key,
        )?;
        let [referenced_column] = foreign_key.ref_columns.as_slice() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY did not resolve exactly one referenced column".into(),
            ));
        };
        let Some(reference) = column.references.as_mut() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY disappeared during validation".into(),
            ));
        };
        reference.referenced_key = foreign_key.referenced_key;
        reference.referenced_index = foreign_key.referenced_index;
        reference.table = foreign_key.ref_table;
        reference.column = Some(referenced_column.clone());
    }
    for foreign_key in &mut c.foreign_keys {
        super::super::foreign_keys::validate_bound_foreign_key_definition_with_local_state(
            context,
            &c.name,
            Some(&local_columns),
            Some(&c.key_constraints),
            foreign_key,
        )?;
    }
    Ok(())
}

pub(super) fn validate_check_expression(
    context: &CreateTableAnalysisContext<'_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let binding = context.bindings.binding_scope()?;
    super::super::constraints::validate_check_expression(
        &SchemaBindingContext {
            catalog: context.schema,
            binding: &binding.context(),
        },
        table,
        qualifier,
        columns,
        expression,
    )
}

/// `ATExecAddConstraint` for each foreign key the statement declares: its name cannot be one a constraint of the table already holds, and then its referenced table is looked up.
fn bind_create_table_relation_references(
    catalog: &dyn super::super::foreign_keys::ForeignKeyDefinitionCatalog,
    table: &mut CreateTable,
    inherited: &InheritedDefinitions,
) -> Result<(), SQLError> {
    let table_name = table.name.clone();
    let qualifier = table.qualifier.clone();
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(&table_name).map_err(SQLError::Internal)?;
    let mut held = table
        .columns
        .iter()
        .flat_map(|column| {
            [
                column.not_null_name.clone().filter(|_| column.not_null),
                column.check_name.clone().filter(|_| column.check.is_some()),
            ]
        })
        .chain(table.checks.iter().map(|check| check.name.clone()))
        .chain(table.key_constraints.iter().map(|key| key.name.clone()))
        .chain(
            table.foreign_keys[..inherited.foreign_keys]
                .iter()
                .map(|foreign_key| foreign_key.name.clone()),
        )
        .flatten()
        .collect::<BTreeSet<_>>();
    let mut claim = |name: Option<&String>| match name {
        Some(name) if !held.insert(name.clone()) => Err(
            super::super::check_inheritance::duplicate_check(&relation.name, name),
        ),
        _ => Ok(()),
    };
    for column in &mut table.columns {
        if let Some(reference) = column.references.as_mut() {
            claim(reference.name.as_ref())?;
            bind_create_table_reference(catalog, &table_name, &qualifier, &mut reference.table)?;
        }
    }
    for (position, foreign_key) in table.foreign_keys.iter_mut().enumerate() {
        if position >= inherited.foreign_keys {
            claim(foreign_key.name.as_ref())?;
        }
        bind_create_table_reference(catalog, &table_name, &qualifier, &mut foreign_key.ref_table)?;
    }
    Ok(())
}

fn bind_create_table_reference(
    catalog: &dyn super::super::foreign_keys::ForeignKeyDefinitionCatalog,
    table: &str,
    qualifier: &str,
    reference: &mut String,
) -> Result<(), SQLError> {
    let self_reference = reference == table
        || reference == qualifier
        || table
            .rsplit_once('.')
            .is_some_and(|(_, local_name)| local_name == reference);
    if self_reference {
        table.clone_into(reference);
        return Ok(());
    }
    *reference = catalog.resolve_table_reference(reference)?;
    Ok(())
}
