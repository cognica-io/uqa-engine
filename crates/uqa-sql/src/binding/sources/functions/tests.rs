//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{binding::fixture::empty_binding_context, ColumnType, FunctionTypeResolver};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

struct HostColumns {
    name: String,
}
impl FunctionTypeResolver for HostColumns {
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
impl RoutineResolution for HostColumns {
    fn has_registered_table_function(&self, name: &str) -> bool {
        name == "fixture"
    }
    fn registered_table_function_columns(&self, _: &str) -> Option<Vec<String>> {
        panic!("native binding must use the admitted descriptor producer")
    }
    fn registered_table_function_columns_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        assert_eq!(name, "fixture");
        let mut columns = ProductionVec::new(*control);
        columns.push_produced(control.copy_text(&self.name)?)?;
        Ok(Some(columns.finish()?))
    }
}
fn source() -> SourcePlan {
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(
        crate::compile("SELECT * FROM fixture()").unwrap().remove(0),
    ) else {
        panic!("query")
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block")
    };
    block.from.unwrap()
}
fn descriptor(
    routines: &HostColumns,
    source: &SourcePlan,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context_with_control(
        empty_binding_context().copy_with_control(control)?,
        control,
    )?
    .bind_function_source(routines, source, &[], &[], None)
}

#[test]
fn function_descriptors_admit_host_names_and_retain_independent_schema_readers() {
    let routines = HostColumns {
        name: "x".repeat(4 << 20),
    };
    let source = source();
    let budget = MemoryBudget::new(64 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let held = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let allocation = allocation_counter::measure(|| {
        let error = descriptor(&routines, &source, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(budget.used(), held.bytes());
    drop(held);
    let schema = descriptor(&routines, &source, &control).unwrap();
    assert_eq!(schema.public_name(0), Some(routines.name.as_str()));
    assert!(!schema.columns_are_open(None));
    let retained = budget.used();
    assert!(retained >= 4 << 20);
    let reader = schema.clone();
    drop(schema);
    assert_eq!(budget.used(), retained);
    drop(reader);
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            descriptor(&routines, &source, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _schema = descriptor(&routines, &source, &control).unwrap();
        panic!("retire result descriptor");
    }));
    assert!(panic.is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn table_result_type_probes_preserve_resource_errors_and_own_selected_type_payloads() {
    let routines = HostColumns {
        name: String::new(),
    };
    let name = "t".repeat(4 << 20);
    let params = [SQLParam::typed_scalar(
        Value::Null,
        ColumnType::Array(Box::new(ColumnType::Named(name.clone()))),
    )];
    let args = [ScalarExpr::Param(1)];
    let columns = ["element".into(), "ordinality".into()];
    let schema = RowSchema::default();
    let budget = MemoryBudget::new(32 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let produce = || {
        crate::semantics::table_function_column_types_with_control(
            &routines,
            TableFunctionTypeRequest {
                name: "unnest",
                binding: None,
                args: &args,
                user_function: None,
                user_invocation: None,
                declared_types: &[],
                columns: &columns,
                ordinality: true,
            },
            &schema,
            &params,
            &routines,
            &control,
        )
    };
    let held = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(produce().unwrap_err().sqlstate(), Some("53200"));
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(budget.used(), held.bytes());
    drop(held);
    let types = produce().unwrap();
    assert_eq!(types[0], Some(ColumnType::Named(name)));
    assert_eq!(types[1], Some(ColumnType::BigInteger));
    assert!(budget.used() >= 4 << 20 && budget.used() < (4 << 20) + (64 << 10));
    drop(types);
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(produce().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}
