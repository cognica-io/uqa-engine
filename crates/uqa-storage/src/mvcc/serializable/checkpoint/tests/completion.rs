//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The last provider read remains inside controlled checkpoint admission.

use super::*;

struct CancelAtEnd<'a> {
    input: &'a [u8],
    control: &'a StorageReadControl,
}

impl Read for CancelAtEnd<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = self.input.read(output)?;
        if count == 0 {
            self.control.cancellation().cancel();
        }
        Ok(count)
    }
}

#[test]
fn cancellation_on_the_final_reader_call_prevents_legacy_checkpoint_admission() {
    let (mut graph, control) = setup();
    let actor = graph.admit(false, &control).unwrap();
    graph
        .observe_read(actor, point(b"retained"), &control)
        .unwrap();
    let encoded = encode(&graph, &control);
    drop(graph);
    let mut input = CancelAtEnd {
        input: &encoded,
        control: &control,
    };
    let result = SerializableGraph::read_checkpoint(DATABASE, COORDINATOR, &mut input, &control);
    let error = result.err().expect("cancelled checkpoint completion");
    assert!(matches!(
        error.into_storage_error(),
        crate::StorageBackendError::Cancelled(_)
    ));
    assert_eq!(control.memory().used(), 0);
}
