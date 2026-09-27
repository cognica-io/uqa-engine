//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::relation_lock_support::after_wait;

#[test]
fn diskann_explain_keeps_the_described_index_until_transaction_end() {
    for provider in 0..3 {
        for statement in [
            "SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
            "UPDATE diskann_docs SET id=id+1 WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
            "CREATE TABLE explained_copy AS SELECT id FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0],1)",
        ] {
            let (_directory, engine, peer) = sessions(provider);
            fixture(&engine);
            sql(&engine, "BEGIN");
            let plan = explain(&engine, statement);
            assert_eq!(nodes(&plan).len(), 1);
            let (_peer, result) = after_wait(
                &engine,
                peer,
                "DROP INDEX diskann_idx",
                "public.diskann_docs",
                "ROLLBACK",
            );
            result.unwrap();
        }
    }
}
