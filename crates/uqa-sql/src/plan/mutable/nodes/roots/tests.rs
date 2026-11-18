//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

struct Fields<'a>(AsyncImageInput<'a>);
impl<'a> Input for Fields<'a> {
    type Work = AsyncImageInput<'a>;
    fn work(&mut self) -> &mut Self::Work {
        &mut self.0
    }
    async fn scalar(
        &mut self,
        _: &mut ScalarExpr,
        _: Option<&mut Option<ScalarMemory>>,
        _: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        panic!("empty scalar containers do not invoke the scalar consumer")
    }
}

type Decoder = ImageDecoder<(), SQLError>;

fn input(
    kind: usize,
    count: usize,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Decoder {
    let sql = match kind {
        0 => "VALUES (1)",
        1 => "SELECT 1",
        _ => "SELECT 1 FROM (VALUES (1)) t",
    };
    let UnifiedPlan::Query(mut plan) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query fixture")
    };
    match &mut plan.root {
        RelationalPlan::Values { rows, .. } => *rows = vec![Vec::new(); count],
        RelationalPlan::QueryBlock(block) => {
            block.projections.clear();
            if kind == 1 {
                block.grouping_sets = vec![Vec::new(); count];
            } else {
                let Some(SourcePlan::Values { rows, .. }) = &mut block.from else {
                    panic!("source values fixture")
                };
                *rows = vec![Vec::new(); count];
            }
        }
        _ => unreachable!(),
    }
    let original = original.clone();
    let native = native.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut fields = Fields(reader.input());
        let control = ProductionControl::new(&native, &original, fields.0.cancellation);
        if kind == 2 {
            let RelationalPlan::QueryBlock(block) = &mut plan.root else {
                unreachable!()
            };
            source(block.from.as_mut().unwrap(), None, &control, &mut fields).await
        } else {
            query(
                &mut plan,
                None,
                QueryScope::WindowUses,
                &control,
                &mut fields,
            )
            .await
        }
    })
    .unwrap()
}

fn advance(input: &mut Decoder) -> Result<Option<()>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn scalar_free_query_and_source_containers_consume_bounded_work() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for kind in 0..3 {
        let mut reader = input(kind, 4096, &native, &work, &token, &token);
        let mut polls = 0;
        loop {
            polls += 1;
            let mut output = None;
            let allocation = allocation_counter::measure(|| output = Some(advance(&mut reader)));
            assert_eq!(
                allocation.count_total, 0,
                "kind={kind}, poll={polls}: {allocation:?}"
            );
            if output.unwrap().unwrap().is_some() {
                break;
            }
            assert!(polls < 10_000, "scalar field traversal stopped progressing");
        }
        assert!(
            polls >= 4096,
            "kind={kind}: {polls} polls for 4096 empty containers"
        );
        drop(reader);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn scalar_free_containers_preserve_cancellation_and_abandonment_at_every_yield() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let sibling = native.reserve(37).unwrap();
    for kind in 0..3 {
        let mut complete = input(kind, 16, &native, &work, &token, &token);
        let mut pending = 0;
        while advance(&mut complete).unwrap().is_none() {
            pending += 1;
        }
        drop(complete);
        for prefix in 0..=pending {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut reader = input(kind, 16, &native, &work, &original, &invoking);
                for _ in 0..prefix {
                    assert!(advance(&mut reader).unwrap().is_none());
                }
                if cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
                }
                let retired = allocation_counter::measure(|| drop(reader));
                assert_eq!(retired.count_total, 0, "{retired:?}");
                assert_eq!(
                    (native.used(), work.used()),
                    (37, 0),
                    "kind={kind}, prefix={prefix}, cancellation={cancellation}"
                );
            }
        }
    }
    drop(sibling);
}
