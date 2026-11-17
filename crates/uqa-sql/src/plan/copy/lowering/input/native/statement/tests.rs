//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::work::Work,
    plan::{MetadataObserver, NoRegisteredAggregates, ProjectionAliasUpdates, ProjectionPlan},
    ScalarExpr,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionVec},
    CancellationToken,
};

struct Labels(usize);
impl<W: Work + Send> MetadataObserver<W> for Labels {
    async fn projections(
        &mut self,
        projections: &[ProjectionPlan],
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<Option<ProjectionAliasUpdates>> {
        let mut output = ProductionVec::new(*control);
        for (index, projection) in projections.iter().enumerate() {
            work.checkpoint().await?;
            control.check()?;
            self.0 += 1;
            if let ScalarExpr::Column(name) = &projection.expr {
                let name = work.text(name, control).await?;
                output
                    .push_produced(control.finish((index, name), control.empty_reservation())?)?;
            }
        }
        Ok(Some(output.finish()?))
    }
}

#[test]
fn metadata_traversal_bounds_deep_and_wide_work_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            for deep in [false, true] {
                let mut source = crate::compile("SELECT marker").unwrap().remove(0);
                let expected = if deep {
                    for _ in 0..512 {
                        let Statement::Select(child) = source else {
                            unreachable!()
                        };
                        let Statement::Select(mut parent) =
                            crate::compile("SELECT 1").unwrap().remove(0)
                        else {
                            unreachable!()
                        };
                        parent.projections[0].expr = crate::ast::Expr::ScalarSubquery(child);
                        source = Statement::Select(parent);
                    }
                    513
                } else {
                    let Statement::Select(query) = &mut source else {
                        unreachable!()
                    };
                    query.projections = vec![query.projections[0].clone(); 4096];
                    4096
                };
                let source = Arc::new(source);
                let budget = MemoryBudget::new(128 << 20);
                let work = MemoryBudget::new(1 << 20);
                let token = CancellationToken::new();
                let owner = budget.clone();
                let original = token.clone();
                let mut decoder = ImageDecoder::new(&work, &token, |mut reader| async move {
                    let mut channel = reader.input();
                    let control = ProductionControl::new(&owner, &original, channel.cancellation);
                    let mut plan = UnifiedPlan::lower_executable_from_input(
                        &source,
                        &NoRegisteredAggregates,
                        &control,
                        &mut channel,
                    )
                    .await?;
                    let mut labels = Labels(0);
                    plan.0
                        .apply_metadata_from(&mut labels, &control, &mut channel)
                        .await?;
                    Ok::<_, SQLError>((plan, labels.0))
                })
                .unwrap();
                let mut polls = 0;
                loop {
                    polls += 1;
                    let output = decoder
                        .advance(
                            &mut std::io::empty(),
                            ImageDecodeWindow { bytes: 1, steps: 1 },
                        )
                        .unwrap();
                    if let Some((plan, count)) = output {
                        assert_eq!(count, expected);
                        assert_eq!(allocation_counter::measure(|| drop(plan)).count_total, 0);
                        break;
                    }
                    assert!(polls < 1_000_000);
                }
                assert!(polls > 512);
                drop(decoder);
                assert_eq!((budget.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
