//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::partition_bound_order;
use crate::ast::{
    ColumnDef, ColumnType, Expr, FunctionBinding, PartitionBound, PartitionRangeDatum,
    PartitionSpec, PartitionStrategy, TableHierarchy,
};
use crate::semantics::partition::{
    partition_tree, PartitionCatalog, PartitionContext, PartitionExpressions,
};
use crate::type_resolution::FunctionTypeResolver;
use crate::{ResultRow, RowSchema, SQLError, SQLParam};
use std::collections::BTreeMap;
use uqa_core::Value;

struct Literals;

impl PartitionExpressions for Literals {
    fn evaluate_bound(&self, expression: &Expr, _params: &[SQLParam]) -> Result<Value, SQLError> {
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            other => Err(SQLError::Internal(format!("not a literal: {other:?}"))),
        }
    }

    fn evaluate_row(
        &self,
        _expression: &Expr,
        _row: &ResultRow,
        _schema: &RowSchema,
        _params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        Err(SQLError::Internal("rows are not evaluated".into()))
    }
}

struct NoFunctions;

impl FunctionTypeResolver for NoFunctions {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

/// Tables by name, each with its hierarchy, in a list-partitioned `root` whose partition `ranged` is range-partitioned.
struct Catalog(BTreeMap<String, TableHierarchy>);

impl PartitionCatalog for Catalog {
    fn try_table_hierarchy(&self, table: &str) -> Result<TableHierarchy, String> {
        self.0
            .get(table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))
    }

    fn direct_hierarchy_children(&self, parent: &str) -> Result<Vec<String>, SQLError> {
        Ok(self
            .0
            .iter()
            .filter(|(_, hierarchy)| hierarchy.parents.first().map(String::as_str) == Some(parent))
            .map(|(name, _)| name.clone())
            .collect())
    }

    fn try_resolve_table_name(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.contains_key(name).then(|| name.to_string()))
    }

    fn try_describe_table(&self, _table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }

    fn try_table_object_id(&self, table: &str) -> Result<Option<[u8; 16]>, String> {
        Ok(self
            .0
            .keys()
            .position(|name| name == table)
            .map(|position| [u8::try_from(position).unwrap(); 16]))
    }
}

fn int(value: i64) -> Expr {
    Expr::Literal(Value::Int(value))
}

fn list(values: Vec<Expr>) -> PartitionBound {
    PartitionBound::List(values)
}

fn range(lower: PartitionRangeDatum, upper: PartitionRangeDatum) -> PartitionBound {
    PartitionBound::Range {
        lower: vec![lower],
        upper: vec![upper],
    }
}

/// Bound ordering reads no domain, assignment or schema expression.
struct Unused;

impl crate::expr::EngineHook for Unused {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!("bound ordering executes no sequence")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!("bound ordering executes no sequence")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!("bound ordering executes no sequence")
    }
}

impl crate::catalog::domain::DomainCatalog for Unused {
    fn domain_by_oid(&self, _: u32) -> Option<crate::catalog::domain::StoredDomain> {
        None
    }
}

impl crate::assignment::AssignmentContext for Unused {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        _: &ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        unreachable!("bound ordering checks no domain")
    }
}

impl FunctionTypeResolver for Unused {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        unreachable!("bound ordering resolves no function")
    }
}

impl crate::routines::RoutineResolution for Unused {}

impl crate::plan::AggregateClassifier for Unused {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

impl crate::schema::SchemaExpressionCatalog for Unused {
    fn plan_schema_expression(
        &self,
        _: &crate::ast::Expr,
        _: &[crate::ast::ColumnDef],
    ) -> std::result::Result<crate::schema::expressions::PlannedSchemaExpression, crate::SQLError>
    {
        unreachable!("this fixture does not plan stored expressions")
    }

    fn registered_runtime_function_volatility(
        &self,
        _: &str,
    ) -> Option<crate::ast::FunctionVolatility> {
        None
    }
    fn schema_expression_columns(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, SQLError> {
        Ok(None)
    }
}

fn context(catalog: &Catalog) -> PartitionContext<'_> {
    PartitionContext {
        catalog,
        expressions: &Literals,
        types: &NoFunctions,
        assignment: &Unused,
        schema: &Unused,
    }
}

#[test]
fn list_partitions_follow_their_smallest_value_then_null_then_default() {
    let catalog = Catalog(BTreeMap::new());
    let order = partition_bound_order(
        &context(&catalog),
        vec![
            ("default".into(), PartitionBound::Default),
            ("null".into(), list(vec![Expr::Literal(Value::Null)])),
            ("fifty".into(), list(vec![int(50)])),
            ("five".into(), list(vec![int(20), int(5)])),
            ("ten".into(), list(vec![int(10)])),
        ],
    )
    .unwrap();
    assert_eq!(order, ["five", "ten", "fifty", "null", "default"]);
}

#[test]
fn range_and_hash_partitions_follow_their_bounds() {
    let catalog = Catalog(BTreeMap::new());
    let ranges = partition_bound_order(
        &context(&catalog),
        vec![
            (
                "second".into(),
                range(
                    PartitionRangeDatum::Value(int(10)),
                    PartitionRangeDatum::Value(int(20)),
                ),
            ),
            (
                "first".into(),
                range(
                    PartitionRangeDatum::MinValue,
                    PartitionRangeDatum::Value(int(10)),
                ),
            ),
            (
                "third".into(),
                range(
                    PartitionRangeDatum::Value(int(20)),
                    PartitionRangeDatum::MaxValue,
                ),
            ),
        ],
    )
    .unwrap();
    assert_eq!(ranges, ["first", "second", "third"]);
    let hashes = partition_bound_order(
        &context(&catalog),
        [(4, 1), (2, 1), (4, 3), (2, 0)]
            .into_iter()
            .map(|(modulus, remainder)| {
                (
                    format!("{modulus}_{remainder}"),
                    PartitionBound::Hash { modulus, remainder },
                )
            })
            .collect(),
    )
    .unwrap();
    assert_eq!(hashes, ["2_0", "2_1", "4_1", "4_3"]);
}

#[test]
fn a_partition_tree_lists_parents_before_their_partitions_in_partition_order() {
    let partitioned = |strategy| PartitionSpec {
        strategy,
        keys: vec![Expr::Column("a".into())],
    };
    let partition = |parent: &str, bound| TableHierarchy {
        parents: vec![parent.into()],
        partition_bound: Some(bound),
        ..TableHierarchy::default()
    };
    let mut ranged = partition("root", list(vec![int(1)]));
    ranged.partition_spec = Some(partitioned(PartitionStrategy::Range));
    let catalog = Catalog(BTreeMap::from([
        (
            "root".into(),
            TableHierarchy {
                partition_spec: Some(partitioned(PartitionStrategy::List)),
                ..TableHierarchy::default()
            },
        ),
        (
            "a_default".into(),
            partition("root", PartitionBound::Default),
        ),
        ("b_two".into(), partition("root", list(vec![int(2)]))),
        ("ranged".into(), ranged),
        (
            "ranged_high".into(),
            partition(
                "ranged",
                range(
                    PartitionRangeDatum::Value(int(10)),
                    PartitionRangeDatum::MaxValue,
                ),
            ),
        ),
        (
            "ranged_low".into(),
            partition(
                "ranged",
                range(
                    PartitionRangeDatum::MinValue,
                    PartitionRangeDatum::Value(int(10)),
                ),
            ),
        ),
    ]));
    let tree = partition_tree(&context(&catalog), "root", false).unwrap();
    assert_eq!(
        tree.iter()
            .map(|node| (node.table.as_str(), node.parent.as_str()))
            .collect::<Vec<_>>(),
        [
            ("ranged", "root"),
            ("ranged_low", "ranged"),
            ("ranged_high", "ranged"),
            ("b_two", "root"),
            ("a_default", "root"),
        ]
    );
    let subtree = partition_tree(&context(&catalog), "ranged", true).unwrap();
    assert_eq!(
        subtree
            .iter()
            .map(|node| node.table.as_str())
            .collect::<Vec<_>>(),
        ["ranged", "ranged_low", "ranged_high"]
    );
}
