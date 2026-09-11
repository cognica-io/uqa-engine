//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::RefCell;
use uqa_sql::{ast::Statement, plan::ExpressionPlan, retrieval::AttentionSpec};

#[derive(Default)]
struct Inputs {
    events: RefCell<Vec<String>>,
}

impl EngineHook for Inputs {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence operation")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence operation")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("unexpected sequence operation")
    }
    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value, SQLError>> {
        self.events.borrow_mut().push(name.into());
        Some(Ok(Value::Str("search".into())))
    }
}
impl GraphNameCatalog for Inputs {
    fn list_graphs(&self) -> Result<Vec<String>, SQLError> {
        self.events.borrow_mut().push("graphs".into());
        Ok(vec!["citations".into()])
    }
}

fn predicate(sql: &str) -> ScalarExpr {
    let Statement::Select(statement) = uqa_sql::compile(sql).unwrap().remove(0) else {
        panic!("expected SELECT")
    };
    ExpressionPlan::lower(statement.r#where.unwrap()).scalar
}

#[test]
fn logical_attention_binds_callbacks_once_before_physical_model_construction() {
    let inputs = Inputs::default();
    let binding = RetrievalBinding {
        hook: &inputs,
        graphs: &inputs,
    };
    let expression = predicate("SELECT * FROM docs WHERE fuse_multihead(bayesian_match(body, query_text()), knn_match(embedding, ARRAY[1.0, 0.0], 2), n_heads => 3, normalized => true, alpha => 0.4)");
    let logical = retrieval::lower_where_bound(
        &binding,
        &expression,
        &RetrievalConstants {
            params: &[],
            evaluate: &evaluate_constant,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(*inputs.events.borrow(), ["query_text"]);
    assert!(
        matches!(&logical, retrieval::RetrievalExpr::AttentionFusion {
        options: AttentionSpec::MultiHead { n_heads: 3, normalized: true, alpha }, ..
    } if *alpha == 0.4)
    );
    let OperatorTree::AttentionFusion {
        attention,
        signals,
        query_features,
    } = instantiate(logical).unwrap()
    else {
        panic!("expected physical attention")
    };
    assert_eq!(*inputs.events.borrow(), ["query_text"]);
    assert_eq!(attention.head_count(), 3);
    assert_eq!(attention.alpha(), 0.4);
    assert!(attention.normalize());
    assert!(query_features.is_empty());
    assert!(matches!(&signals[1], OperatorTree::CosineProbability(_)));
    assert!(attention
        .fuse(&[0.8, 0.6], &[0.0; uqa_fusion::N_QUERY_FEATURES])
        .unwrap()
        .is_finite());
}

#[test]
fn retrieval_validation_precedes_runtime_argument_effects() {
    let inputs = Inputs::default();
    let binding = RetrievalBinding {
        hook: &inputs,
        graphs: &inputs,
    };
    let ScalarExpr::Func { args, .. } =
        predicate("SELECT * FROM docs WHERE knn_match(embedding, query_vector())")
    else {
        panic!("expected call")
    };
    assert!(matches!(
        binding.lower_function("knn_match", &args, &[]),
        Err(SQLError::BadArity { actual: 2, .. })
    ));
    let expression = predicate("SELECT * FROM docs WHERE fuse_bayesian_evidence(text_match(body, query_text()), knn_match(embedding, ARRAY[1.0, 0.0], 2))");
    assert!(
        matches!(binding.lower_where(&expression, &[]), Err(SQLError::TypeMismatch(message)) if message.contains("requires probability-valued signals"))
    );
    assert!(inputs.events.borrow().is_empty());
}

#[test]
fn graph_default_resolution_retains_row_call_and_predicate_evaluation_order() {
    let inputs = Inputs::default();
    let binding = RetrievalBinding {
        hook: &inputs,
        graphs: &inputs,
    };
    let expression = predicate("SELECT * FROM docs WHERE rpq(query_text(), 'invalid start')");
    let ScalarExpr::Func { args, .. } = &expression else {
        panic!("expected call")
    };
    assert!(
        matches!(binding.lower_function("rpq", args, &[]), Err(SQLError::TypeMismatch(message)) if message.contains("arguments cannot be lowered"))
    );
    assert_eq!(*inputs.events.borrow(), ["query_text", "graphs"]);
    inputs.events.borrow_mut().clear();
    assert!(
        matches!(binding.lower_where(&expression, &[]), Err(SQLError::TypeMismatch(message)) if message == "rpq.expr must be a constant string")
    );
    assert_eq!(*inputs.events.borrow(), ["graphs"]);
}

#[test]
fn logical_attention_defers_checked_model_capacity_to_execution() {
    let inputs = Inputs::default();
    let binding = RetrievalBinding {
        hook: &inputs,
        graphs: &inputs,
    };
    // This exceeds Vec's byte-capacity limit before any allocation is attempted.
    let heads = (isize::MAX as usize / std::mem::size_of::<uqa_fusion::AttentionFusion>()) + 1;
    let expression = predicate(&format!("SELECT * FROM docs WHERE fuse_multihead(bayesian_match(body, 'rust'), knn_match(embedding, ARRAY[1.0, 0.0], 2), n_heads => {heads})"));
    let logical = retrieval::lower_where_bound(
        &binding,
        &expression,
        &RetrievalConstants {
            params: &[],
            evaluate: &evaluate_constant,
        },
    )
    .unwrap()
    .unwrap();
    assert!(
        matches!(&logical, retrieval::RetrievalExpr::AttentionFusion {
        options: AttentionSpec::MultiHead { n_heads, .. }, ..
    } if *n_heads == heads)
    );
    assert!(inputs.events.borrow().is_empty());
    assert!(
        matches!(instantiate(logical), Err(SQLError::TypeMismatch(message))
        if message == "fuse_multihead: multi-head attention head count exceeds available memory")
    );
    assert!(inputs.events.borrow().is_empty());
}

#[test]
fn direct_vector_pool_is_owned_only_by_the_complete_root_predicate() {
    use crate::query::table_sources::retrieval::DirectVectorRetrieval;

    let inputs = Inputs::default();
    let binding = RetrievalBinding {
        hook: &inputs,
        graphs: &inputs,
    };
    let knn = predicate("SELECT * FROM docs WHERE knn_match(embedding, ARRAY[0.9, 0.1], 3)");
    assert!(matches!(
        binding.direct_vector_retrieval(&knn, &[]).unwrap(),
        Some(DirectVectorRetrieval::Knn { top_k: 3 })
    ));
    let calibrated = predicate(
        "SELECT * FROM docs WHERE calibrated_vector_match('embedding', ARRAY[0.9, 0.1], 3, 0.6)",
    );
    assert!(
        matches!(binding.direct_vector_retrieval(&calibrated, &[]).unwrap(),
        Some(DirectVectorRetrieval::Calibrated { field, query_vector, top_k: 3, threshold: Some(threshold) })
        if field == "embedding" && query_vector == [0.9_f32, 0.1_f32] && threshold == 0.6)
    );
    let bounded = predicate(
        "SELECT * FROM docs WHERE knn_match(embedding, ARRAY[0.9, 0.1], 3) AND category = 1",
    );
    assert!(binding
        .direct_vector_retrieval(&bounded, &[])
        .unwrap()
        .is_none());
    assert!(inputs.events.borrow().is_empty());
}
