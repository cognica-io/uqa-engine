//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed vector models are checked against the retained retrieval surface before search.

use crate::storage_errors::storage_error;
use std::fmt::Write;
use uqa_scoring::{VectorCalibrationModel, VectorCalibrationTarget};
use uqa_sql::SQLError;
use uqa_storage::{read_control::StorageReadControl, VectorIndex};

#[cfg(test)]
mod tests;

pub fn validate_names(
    model: &VectorCalibrationModel,
    target: &VectorCalibrationTarget,
    table: &str,
    field: &str,
) -> Result<(), SQLError> {
    model
        .validate_for(target)
        .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
    if target.corpus_id != table {
        return Err(SQLError::TypeMismatch(format!(
            "vector calibration corpus_id {:?} does not match table {:?}",
            target.corpus_id, table
        )));
    }
    let expected = format!("{table}.{field}");
    if target.index_id != expected {
        return Err(SQLError::TypeMismatch(format!(
            "vector calibration index_id {:?} does not match physical index {:?}",
            target.index_id, expected
        )));
    }
    Ok(())
}

fn version(prefix: &str, fingerprint: [u8; 32]) -> String {
    let mut value = String::with_capacity(prefix.len() + 64);
    value.push_str(prefix);
    for byte in fingerprint {
        write!(value, "{byte:02x}").expect("formatting into a string");
    }
    value
}

fn diskann_versions(
    index: &dyn VectorIndex,
    control: &StorageReadControl,
) -> Result<(String, String), SQLError> {
    let metadata = index
        .diskann_query_metadata(control)
        .map_err(|error| storage_error("read DiskANN calibration metadata", &error))?
        .ok_or_else(|| {
            SQLError::TypeMismatch(
                "selected index cannot verify DiskANN calibration metadata".into(),
            )
        })?;
    let corpus = metadata.corpus_fingerprint.ok_or_else(|| {
        SQLError::TypeMismatch(
            "selected DiskANN canonical view has no verifiable calibration identity".into(),
        )
    })?;
    let physical = metadata
        .index_fingerprint(control)
        .map_err(|error| storage_error("identify DiskANN calibration generation", &error))?;
    Ok((
        version("uqa-vector-corpus-v1:", corpus),
        version("uqa-diskann-index-v1:", physical),
    ))
}

pub fn validate_index(
    index: &dyn VectorIndex,
    target: &VectorCalibrationTarget,
    control: &StorageReadControl,
) -> Result<(), SQLError> {
    if target.index_kind != index.index_kind() {
        return Err(SQLError::TypeMismatch(format!(
            "vector calibration index kind {:?} does not match {:?}",
            target.index_kind,
            index.index_kind()
        )));
    }
    if target.dimensions != index.dimensions() {
        return Err(SQLError::VectorDimMismatch {
            expected: index.dimensions() as usize,
            actual: target.dimensions as usize,
        });
    }
    if index.index_kind() == "diskann" {
        let (corpus, physical) = diskann_versions(index, control)?;
        if target.corpus_version != corpus || target.index_version != physical {
            return Err(SQLError::TypeMismatch("vector calibration target mismatch: selected DiskANN corpus or physical generation changed".into()));
        }
    }
    Ok(())
}

pub fn diskann_target(
    index: &dyn VectorIndex,
    table: &str,
    field: &str,
    embedding: (&str, &str),
    candidate_k: usize,
    control: &StorageReadControl,
) -> Result<VectorCalibrationTarget, SQLError> {
    let (corpus_version, index_version) = diskann_versions(index, control)?;
    let target = VectorCalibrationTarget {
        corpus_id: table.into(),
        corpus_version,
        index_id: format!("{table}.{field}"),
        index_version,
        index_kind: index.index_kind().into(),
        embedding_model_id: embedding.0.into(),
        embedding_model_version: embedding.1.into(),
        candidate_k,
        dimensions: index.dimensions(),
    };
    target
        .validate()
        .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
    Ok(target)
}
