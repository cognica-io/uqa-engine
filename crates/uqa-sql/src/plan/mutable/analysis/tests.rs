//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::work::inline,
    plan::{
        retirement::construction::{Context, Prefix},
        PlanRef, RelationalPlan, UnifiedPlan,
    },
    ScalarExpr,
};
use std::collections::BTreeSet;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionVec},
    CancellationToken,
};

mod layouts;

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

#[derive(Default, Debug)]
struct Names {
    visited: Vec<String>,
    identities: Option<BTreeSet<usize>>,
    fail_at: Option<usize>,
}

impl Names {
    fn capture(plan: PlanRef<'_>) -> Self {
        let mut identities = BTreeSet::new();
        plan.visit_scalar_expressions(&mut |expression| {
            expression.visit(&mut |expression| {
                identities.insert(std::ptr::from_ref(expression).addr());
            });
        });
        Self {
            identities: Some(identities),
            ..Self::default()
        }
    }
}

impl<W: Work + Send> MetadataObserver<W> for Names {
    fn visit_table_queries(&self) -> bool {
        true
    }

    async fn projections(
        &mut self,
        projections: &[ProjectionPlan],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<ProjectionAliasUpdates>, SQLError> {
        let mut names = ProductionVec::new(*control);
        for (index, projection) in projections.iter().enumerate() {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            if let Some(identities) = &self.identities {
                assert!(identities.contains(&std::ptr::from_ref(&projection.expr).addr()));
            }
            if let ScalarExpr::Column(name) = &projection.expr {
                if self.fail_at == Some(self.visited.len()) {
                    return Err(SQLError::Internal("selected metadata failure".into()));
                }
                self.visited.push(name.to_string());
                let alias = work.text(name, control).await?;
                names
                    .push_produced(control.finish((index, alias), control.empty_reservation())?)?;
            }
        }
        Ok(Some(names.finish()?))
    }
}

#[test]
fn metadata_traversal_preserves_original_slots_and_statement_order() {
    let fixtures: &[(&str, &[&str])] = &[
        ("WITH a AS (SELECT cte) SELECT outer_col,(SELECT scalar_col) FROM (SELECT inner_col) q JOIN t ON true", &["cte", "outer_col", "inner_col", "scalar_col"]),
        ("WITH a AS (SELECT cte) SELECT left_col UNION ALL SELECT right_col", &["cte", "left_col", "right_col"]),
        ("VALUES ((SELECT first_col)), ((SELECT second_col))", &["first_col", "second_col"]),
        ("WITH a AS (SELECT cte) INSERT INTO t SELECT input_col,(SELECT scalar_col) RETURNING return_col", &["return_col", "cte", "input_col", "scalar_col"]),
        ("WITH a AS (SELECT cte) UPDATE t SET n=(SELECT scalar_col) FROM (SELECT source_col) s RETURNING return_col", &["return_col", "cte", "scalar_col", "source_col"]),
        ("WITH a AS (SELECT cte) DELETE FROM t USING (SELECT source_col) s WHERE n=(SELECT scalar_col) RETURNING return_col", &["return_col", "cte", "scalar_col", "source_col"]),
        ("MERGE INTO t USING (SELECT source_col) s ON true WHEN MATCHED THEN UPDATE SET n=(SELECT scalar_col) RETURNING return_col", &["return_col", "scalar_col", "source_col"]),
        ("WITH a AS (DELETE FROM t RETURNING cte_col) SELECT outer_col FROM a", &["cte_col", "outer_col"]),
        ("EXPLAIN SELECT first_col,(SELECT second_col)", &["first_col", "second_col"]),
        ("PREPARE p AS SELECT first_col,(SELECT second_col)", &["first_col", "second_col"]),
        ("CREATE VIEW v AS SELECT first_col,(SELECT second_col)", &["first_col", "second_col"]),
        ("CREATE MATERIALIZED VIEW v AS SELECT first_col", &["first_col"]),
        ("CREATE TABLE t AS SELECT first_col", &["first_col"]),
        ("DECLARE c CURSOR FOR SELECT first_col", &["first_col"]),
        ("CALL p((SELECT first_col),(SELECT second_col))", &["first_col", "second_col"]),
        ("EXECUTE p((SELECT first_col),(SELECT second_col))", &["first_col", "second_col"]),
    ];
    for (sql, expected) in fixtures {
        let budget = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        for controlled in [false, true] {
            let control = if controlled {
                ProductionControl::new(&budget, &token, &token)
            } else {
                ProductionControl::uncontrolled()
            };
            let mut destination = plan(sql).copy_for_rewrite(&control).unwrap();
            let mut observer = Names::capture(destination.as_plan_ref());
            inline(destination.apply_metadata_from(&mut observer, &control, &mut ())).unwrap();
            assert_eq!(observer.visited, *expected, "{sql}");
            drop(destination);
            assert_eq!(budget.used(), 0, "{sql}");
        }
    }
}

type Output = (Prefix<MutableExecutablePlan>, Names);
type Decoder = ImageDecoder<Output, SQLError>;

fn retained(
    sql: &str,
    budget: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Prefix<MutableExecutablePlan> {
    let control = ProductionControl::new(budget, original, invoking);
    let mut context = Context::new(&control).unwrap();
    for _ in 0..16 {
        context = context.child(&control).unwrap();
    }
    context.retain(plan(sql).copy_for_rewrite(&control).unwrap())
}

fn input(
    mut plan: Prefix<MutableExecutablePlan>,
    budget: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    fail_at: Option<usize>,
    deny: bool,
) -> Decoder {
    let budget = budget.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&budget, &original, channel.cancellation);
        let mut observer = Names::capture(plan.as_plan_ref());
        observer.fail_at = fail_at;
        let _held = deny.then(|| budget.reserve(budget.available()).unwrap());
        plan.apply_metadata_from(&mut observer, &control, &mut channel)
            .await?;
        Ok((plan, observer))
    })
    .unwrap()
}

fn step(input: &mut Decoder) -> Result<Option<Output>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn metadata_traversal_retires_every_interruption_and_failed_update() {
    let sql = "WITH a AS (SELECT first_col) SELECT second_col,(SELECT third_col) FROM (SELECT fourth_col) q";
    let budget = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut complete = input(
        retained(sql, &budget, &token, &token),
        &budget,
        &work,
        &token,
        &token,
        None,
        false,
    );
    let mut polls = 0;
    loop {
        polls += 1;
        if let Some((plan, observer)) = step(&mut complete).unwrap() {
            assert_eq!(
                observer.visited,
                ["first_col", "second_col", "fourth_col", "third_col"]
            );
            drop((plan, observer));
            break;
        }
        assert!(polls < 1000);
    }
    drop(complete);
    let sibling = budget.reserve(37).unwrap();
    for prefix in 0..polls {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut decoder = input(
                retained(sql, &budget, &original, &invoking),
                &budget,
                &work,
                &original,
                &invoking,
                None,
                false,
            );
            for _ in 0..prefix {
                assert!(step(&mut decoder).unwrap().is_none());
            }
            match cancel {
                1 => original.cancel(),
                2 => invoking.cancel(),
                _ => (),
            }
            if cancel != 0 {
                assert_eq!(step(&mut decoder).unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(allocation_counter::measure(|| drop(decoder)).count_total, 0);
            assert_eq!((budget.used(), work.used()), (sibling.bytes(), 0));
        }
    }
    for (fail_at, deny) in [(Some(2), false), (None, true)] {
        let decoder = input(
            retained(sql, &budget, &token, &token),
            &budget,
            &work,
            &token,
            &token,
            fail_at,
            deny,
        );
        let error = decoder.read(&mut std::io::empty()).unwrap_err();
        if deny {
            assert_eq!(error.sqlstate(), Some("53200"));
        } else {
            assert_eq!(
                error.to_string(),
                "internal error: selected metadata failure"
            );
        }
        assert_eq!((budget.used(), work.used()), (sibling.bytes(), 0));
    }
}

#[test]
fn metadata_traversal_visits_replacements_and_keeps_table_query_selection() {
    let budget = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut destination = plan("SELECT old_col").copy_for_rewrite(&control).unwrap();
    let UnifiedPlan::Query(query) = plan("SELECT new_col,(SELECT nested_col)") else {
        unreachable!()
    };
    let mut replacement = Some(query.copy_for_rewrite(&control).unwrap());
    // The existing synchronous callback remains usable with non-Send state.
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    destination
        .apply_query_metadata(&control, &mut |_| {
            calls.set(calls.get() + 1);
            Ok(QueryMetadataUpdate {
                root: replacement.take().map(Into::into),
                ..Default::default()
            })
        })
        .unwrap();
    assert_eq!(calls.get(), 2);
    let mut names = Names::default();
    inline(destination.apply_metadata_from(&mut names, &control, &mut ())).unwrap();
    assert_eq!(names.visited, ["new_col", "nested_col"]);
    drop(destination);

    let mut source = plan("SELECT outer_col FROM original");
    let UnifiedPlan::Query(table_body) = plan("SELECT table_col") else {
        unreachable!()
    };
    let UnifiedPlan::Query(query) = &mut source else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let Some(SourcePlan::Table {
        executable_query, ..
    }) = &mut block.from
    else {
        unreachable!()
    };
    *executable_query = Some(table_body);
    let mut destination = source.copy_for_rewrite(&control).unwrap();
    struct Sources(usize);
    impl<W: Work + Send> MetadataObserver<W> for Sources {
        async fn source(
            &mut self,
            _: &SourcePlan,
            control: &ProductionControl<'_>,
            _: &mut W,
        ) -> Result<Option<SourceMetadataUpdate>, SQLError> {
            self.0 += 1;
            Ok(Some(ProductionVec::new(*control).finish()?))
        }
    }
    let mut sources = Sources(0);
    inline(destination.apply_metadata_from(&mut sources, &control, &mut ())).unwrap();
    assert_eq!(sources.0, 1);
    let mut names = Names::default();
    inline(destination.apply_metadata_from(&mut names, &control, &mut ())).unwrap();
    assert_eq!(names.visited, ["outer_col", "table_col"]);
    let UnifiedPlan::Query(mut replacement_query) = plan("SELECT 1 FROM (SELECT source_col) q")
    else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut replacement_query.root else {
        unreachable!()
    };
    let mut replacement = Some(
        block
            .from
            .take()
            .unwrap()
            .copy_for_rewrite(&control)
            .unwrap(),
    );
    destination
        .replace_sources(&control, &mut |source| {
            Ok(if matches!(source, SourcePlan::Table { .. }) {
                replacement.take()
            } else {
                None
            })
        })
        .unwrap();
    let mut names = Names::default();
    inline(destination.apply_metadata_from(&mut names, &control, &mut ())).unwrap();
    assert_eq!(names.visited, ["outer_col", "source_col"]);
    drop(destination);
    assert_eq!(budget.used(), 0);
}
