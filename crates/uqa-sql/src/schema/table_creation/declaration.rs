//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind complete table declarations around their sequence and catalog publication boundaries.
use crate::ast::{ColumnDef, CreateTable, Expr, NotNullDeclaration};
use crate::schema::constraint_metadata::{
    identity::materialize_default_oid, materialize_check_identity, CatalogIdentityAllocator,
    ConstraintMetadataError,
};
use crate::schema::foreign_keys::ForeignKeyDefinitionContext;
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
    let mut declarations = Vec::new();
    let mut primary_keys = Vec::new();
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
                // A column's NOT NULL clause, or the constraint its PRIMARY KEY, SERIAL or identity implies, joins the constraints in the column's position.
                if column.not_null {
                    declarations.push(NotNullDeclaration {
                        column: column.name.clone(),
                        name: column.not_null_name.clone(),
                        no_inherit: column.not_null_no_inherit,
                        explicit: column.not_null_explicit,
                    });
                }
            }
            DeclaredElement::NotNull(declaration) => {
                if target.partitioned && declaration.no_inherit {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "not-null constraints on partitioned tables cannot be NO INHERIT"
                            .into(),
                    });
                }
                declarations.push(declaration.clone());
            }
            DeclaredElement::PrimaryKey { columns } => primary_keys.push(columns.clone()),
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
    super::keys::transform_declared_keys(&context.inheritance, c)?;
    // `transformIndexConstraints` makes each column of a table PRIMARY KEY NOT NULL once every element is examined: a NO INHERIT declaration of the column conflicts, and a column without a declaration takes a generated constraint.
    for columns in primary_keys {
        for column in columns {
            match declarations
                .iter()
                .find(|declaration| declaration.column == column)
            {
                Some(declaration) if declaration.no_inherit => {
                    return Err(SQLError::Routine {
                        sqlstate: "42601".into(),
                        message: format!(
                            "conflicting NO INHERIT declaration for not-null constraint on column \"{column}\""
                        ),
                    });
                }
                Some(_) => {}
                None => declarations.push(NotNullDeclaration {
                    column,
                    name: None,
                    no_inherit: false,
                    explicit: false,
                }),
            }
        }
    }
    c.not_null_declarations = declarations;
    Ok(())
}

/// Describe the new table's columns before its name is claimed: the parents' columns merge ahead of the local ones as `MergeAttributes` merges them, the declared primary key's columns take their NOT NULL constraints, `BuildDescForRelation` requires `USAGE` on every column's type, and `CheckAttributeNamesTypes` rejects system column names and pseudo-types.
pub fn prepare_create_table_declaration(
    context: &CreateTableAnalysisContext<'_>,
    c: &mut CreateTable,
    notices: &mut Vec<crate::SQLNotice>,
) -> Result<InheritedDefinitions, SQLError> {
    let declared = c.key_constraints.len();
    let declared_foreign_keys = c.foreign_keys.len();
    let parents =
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
        expressions: parents.expressions,
        not_nulls: parents.not_nulls,
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

/// Define every declared key before foreign-key binding so a foreign key can reference its own table's keys.
pub fn define_create_table_constraints(
    c: &mut CreateTable,
    indexes: ConstraintIndexNamer<'_>,
    inherited: &InheritedDefinitions,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    super::keys::define_declared_keys(indexes, c, inherited, allocate)
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
