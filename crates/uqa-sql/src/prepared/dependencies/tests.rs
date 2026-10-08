//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn literal(ty: ColumnType, value: Value) -> ScalarExpr {
    ScalarExpr::TypedLiteral {
        composite_source: None,
        value,
        ty: ty.catalog_name(),
        bound_type: Some(ty),
        parameter_index: None,
    }
}

#[test]
fn relation_dependencies_distinguish_scalar_oid_inputs_from_arrays_and_parameters() {
    let mut parameter = literal(ColumnType::Regclass, Value::Int(50));
    if let ScalarExpr::TypedLiteral {
        parameter_index, ..
    } = &mut parameter
    {
        *parameter_index = Some(1);
    }
    let expression = ScalarExpr::Row(vec![
        literal(ColumnType::Regclass, Value::Int(11)),
        literal(ColumnType::Oid, Value::Int(12)),
        ScalarExpr::Array(vec![literal(ColumnType::Regclass, Value::Int(13))]),
        literal(
            ColumnType::Array(Box::new(ColumnType::Regclass)),
            Value::Array(uqa_core::ArrayValue::try_new(vec![Value::Int(40)]).unwrap()),
        ),
        literal(ColumnType::Regproc, Value::Int(41)),
        literal(ColumnType::Regtype, Value::Int(42)),
        literal(ColumnType::Integer, Value::Int(43)),
        literal(ColumnType::Regclass, Value::Null),
        parameter,
    ]);
    let mut dependencies = PreparedAnalysisDependencies::default();
    dependencies.include_expression(&expression);
    assert_eq!(dependencies.relations, BTreeSet::from([11, 12, 13]));
    assert!(dependencies.routines.is_empty());
}

#[derive(Clone)]
struct RetainedIdentity(Arc<()>);

impl PartialEq for RetainedIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RetainedIdentity {}

#[test]
fn opaque_revisions_preserve_exact_equality_and_identity_lifetime() {
    let identity = Arc::new(());
    let retained = Arc::downgrade(&identity);
    let revision = PreparedDependencyRevision::new(RetainedIdentity(identity.clone()));
    let same = PreparedDependencyRevision::new(RetainedIdentity(identity.clone()));
    let different = PreparedDependencyRevision::new(RetainedIdentity(Arc::new(())));
    drop(identity);
    assert_eq!(revision, same);
    assert_ne!(revision, different);
    let cloned = revision.clone();
    drop(revision);
    drop(same);
    assert!(retained.upgrade().is_some());
    drop(cloned);
    assert!(retained.upgrade().is_none());
    assert_eq!(
        PreparedDependencyRevision::new(7_u64),
        PreparedDependencyRevision::new(7_u64)
    );
    assert_ne!(
        PreparedDependencyRevision::new(7_u64),
        PreparedDependencyRevision::new(7_u32)
    );
}

#[test]
fn builtin_bindings_do_not_become_mutable_routine_dependencies() {
    let mut binding = FunctionBinding {
        object_id: Some([3; 16]),
        name: "public.clock".into(),
        argument_types: vec![],
        builtin: false,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
    };
    let mut dependencies = PreparedAnalysisDependencies::default();
    dependencies.include_routine(&binding);
    dependencies.include_routine(&binding);
    binding.builtin = true;
    binding.object_id = Some([4; 16]);
    dependencies.include_routine(&binding);
    assert_eq!(dependencies.routines, BTreeSet::from([[3; 16]]));
}
