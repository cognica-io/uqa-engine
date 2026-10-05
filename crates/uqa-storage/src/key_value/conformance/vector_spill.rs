//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Large evaluated vector journals preserve serial generations through refresh, commit and undo.

use super::{expect, expect_eq};
use crate::{
    read_control::StorageReadControl, HNSWIndex, HNSWIndexParams, IVFIndex, IVFIndexParams,
    PersistentStorageBackend, StorageBackendResult, StorageSavepointId, VectorIndex,
    VectorIndexOpenMode, VectorIndexSpec,
};

const DIMENSIONS: u32 = 512;
const DOCUMENTS: u64 = 320;
const ALLOWANCE: usize = 512 << 10;
const TABLE: &str = "vector_spill";
const FIELD: &str = "embedding";

fn vector(document: u64) -> Vec<f32> {
    let mut value = vec![0.0; DIMENSIONS as usize];
    value[document as usize % DIMENSIONS as usize] = 1.0;
    value[0] = 0.125;
    value
}

fn edits(index: &mut dyn VectorIndex) -> StorageBackendResult<()> {
    index.add(2, vector(80))?;
    index.add_many(3, vec![vector(3), vector(81)])?;
    index.delete(4)?;
    index.add(4, vector(4))?;
    index.add_many(5, Vec::new())
}

fn insert_documents(index: &mut dyn VectorIndex) -> StorageBackendResult<()> {
    for document in 2..=DOCUMENTS + 1 {
        index
            .add(document, vector(document))
            .map_err(|error| crate::StorageBackendError::backend(index.index_kind(), error))?;
    }
    Ok(())
}

fn specification(hnsw: bool) -> VectorIndexSpec {
    if hnsw {
        VectorIndexSpec::HNSW(HNSWIndexParams {
            m: 2,
            ef_construction: 8,
            ef_search: 64,
            rebuild_threshold: 3,
            seed: 7,
        })
    } else {
        VectorIndexSpec::IVF(IVFIndexParams {
            nlist: 2,
            nprobe: 2,
            train_threshold: 8,
        })
    }
}

fn reference(spec: VectorIndexSpec) -> StorageBackendResult<Box<dyn VectorIndex>> {
    Ok(match spec {
        VectorIndexSpec::HNSW(params) => Box::new(HNSWIndex::with_params(DIMENSIONS, params)?),
        VectorIndexSpec::IVF(params) => Box::new(IVFIndex::with_params(
            DIMENSIONS,
            params.nlist,
            params.nprobe,
            params.train_threshold,
        )),
        _ => unreachable!(),
    })
}

/// Run each index kind on a fresh disposable backend. The transaction exceeds its original allowance and must rebase the spilled journal at both command refresh and final publication.
pub fn verify_vector_transaction_spill(
    backend: &dyn PersistentStorageBackend,
    hnsw: bool,
) -> StorageBackendResult<()> {
    let spec = specification(hnsw);
    let mut reference = reference(spec)?;
    let control = StorageReadControl::with_limit(ALLOWANCE);
    {
        let session = backend.open_controlled_session(&control)?;
        session.backend.begin_transaction()?;
        let mut index = session.backend.vector_index(
            TABLE,
            FIELD,
            DIMENSIONS,
            spec,
            VectorIndexOpenMode::Create,
        )?;
        index.add(1, vector(1))?;
        index.initialize()?;
        session.backend.commit_transaction()?;
        reference.add(1, vector(1))?;
        reference.initialize()?;
        let peer_control = StorageReadControl::with_limit(ALLOWANCE);
        let peer = backend.open_controlled_session(&peer_control)?;
        let mut other = peer.backend.vector_index(
            TABLE,
            FIELD,
            DIMENSIONS,
            spec,
            VectorIndexOpenMode::Restore,
        )?;
        session.backend.begin_transaction()?;
        insert_documents(&mut *index)?;
        let keep = StorageSavepointId::allocate();
        session.backend.savepoint(keep)?;
        index.add(350, vector(350))?;
        index.add(2, vector(60))?;
        session.backend.rollback_to_savepoint(keep)?;
        session.backend.release_savepoint(keep)?;
        edits(&mut *index)?;
        other.add(900, vector(900))?;
        session
            .backend
            .refresh_transaction_snapshot(control.cancellation())
            .map_err(|error| {
                crate::StorageBackendError::backend("refreshing spilled vector inputs", error)
            })?;
        other.add(901, vector(901))?;
        session.backend.commit_transaction().map_err(|error| {
            crate::StorageBackendError::backend("committing spilled vector inputs", error)
        })?;
        reference.add(900, vector(900))?;
        reference.add(901, vector(901))?;
        insert_documents(&mut *reference)?;
        edits(&mut *reference)?;
        expect_eq(
            &index.count()?,
            &reference.count()?,
            "spilled ordered replay preserves vector cardinality",
        )?;
        for document in [3, DOCUMENTS + 1, 80, 900, 901] {
            let query = vector(document);
            expect_eq(
                &index.search_knn(&query, 5)?,
                &reference.search_knn(&query, 5)?,
                "spilled rebase matches serial canonical cosine and identity ties",
            )?;
        }
    }
    expect_eq(
        &control.memory().used(),
        &0,
        "closing vector writers releases original allowance",
    )?;
    expect(
        control.memory().peak() <= ALLOWANCE,
        "evaluated inputs and rebase share original ceiling",
    )?;
    expect(
        DOCUMENTS as usize * DIMENSIONS as usize * 4 > ALLOWANCE,
        "raw input bytes exceed the session allowance",
    )?;
    let fresh = backend.open_controlled_session(&control)?;
    let index =
        fresh
            .backend
            .vector_index(TABLE, FIELD, DIMENSIONS, spec, VectorIndexOpenMode::Restore)?;
    expect_eq(
        &index.count()?,
        &reference.count()?,
        "fresh session reads the committed vector population",
    )?;
    expect_eq(
        &index.search_knn(&vector(80), 5)?,
        &reference.search_knn(&vector(80), 5)?,
        "fresh vector owner retains committed rebase scores",
    )
}
