//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL vector-index option aliases, value parsing, and duplicate checks.
use crate::SQLError;
#[derive(Default)]
pub struct IVFIndexOptions {
    pub nlist: Option<usize>,
    pub nprobe: Option<usize>,
    pub train_threshold: Option<usize>,
}
#[derive(Default)]
pub struct HNSWIndexOptions {
    pub m: Option<usize>,
    pub ef_construction: Option<usize>,
    pub ef_search: Option<usize>,
    pub rebuild_threshold: Option<usize>,
    pub seed: Option<u64>,
}
pub fn index_access_method(statement: &crate::ast::CreateIndex) -> Result<String, SQLError> {
    let am = statement.access_method.to_ascii_lowercase();
    if !matches!(am.as_str(), "" | "btree" | "gin" | "ivf" | "hnsw") {
        return Err(SQLError::Unsupported(format!(
            "CREATE INDEX access method `{}` is not supported",
            statement.access_method
        )));
    }

    Ok(am)
}
pub fn parse_ivf_index_options(options: &[(String, String)]) -> Result<IVFIndexOptions, SQLError> {
    let mut params = IVFIndexOptions::default();
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in options {
        if key.eq_ignore_ascii_case("lists") || key.eq_ignore_ascii_case("nlist") {
            claim_index_option(&mut seen, "nlist", "ivf", key)?;
            params.nlist = Some(parse_positive_usize_option("ivf", key, value)?);
        } else if key.eq_ignore_ascii_case("probes") || key.eq_ignore_ascii_case("nprobe") {
            claim_index_option(&mut seen, "nprobe", "ivf", key)?;
            params.nprobe = Some(parse_positive_usize_option("ivf", key, value)?);
        } else if key.eq_ignore_ascii_case("train_threshold")
            || key.eq_ignore_ascii_case("train-threshold")
            || key.eq_ignore_ascii_case("min_train")
        {
            claim_index_option(&mut seen, "train_threshold", "ivf", key)?;
            params.train_threshold = Some(parse_positive_usize_option("ivf", key, value)?);
        } else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING ivf option `{key}` is not supported"
            )));
        }
    }
    Ok(params)
}

pub fn parse_hnsw_index_options(
    options: &[(String, String)],
) -> Result<HNSWIndexOptions, SQLError> {
    let mut params = HNSWIndexOptions::default();
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in options {
        if key.eq_ignore_ascii_case("m") {
            claim_index_option(&mut seen, "m", "hnsw", key)?;
            params.m = Some(parse_positive_usize_option("hnsw", key, value)?);
        } else if key.eq_ignore_ascii_case("ef_construction")
            || key.eq_ignore_ascii_case("ef-construction")
        {
            claim_index_option(&mut seen, "ef_construction", "hnsw", key)?;
            params.ef_construction = Some(parse_positive_usize_option("hnsw", key, value)?);
        } else if key.eq_ignore_ascii_case("ef_search") || key.eq_ignore_ascii_case("ef-search") {
            claim_index_option(&mut seen, "ef_search", "hnsw", key)?;
            params.ef_search = Some(parse_positive_usize_option("hnsw", key, value)?);
        } else if key.eq_ignore_ascii_case("rebuild_threshold")
            || key.eq_ignore_ascii_case("rebuild-threshold")
        {
            claim_index_option(&mut seen, "rebuild_threshold", "hnsw", key)?;
            params.rebuild_threshold = Some(parse_positive_usize_option("hnsw", key, value)?);
        } else if key.eq_ignore_ascii_case("seed") {
            claim_index_option(&mut seen, "seed", "hnsw", key)?;
            params.seed = Some(value.parse::<u64>().map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "CREATE INDEX USING hnsw option `{key}` must be an unsigned integer"
                ))
            })?);
        } else {
            return Err(SQLError::Unsupported(format!(
                "CREATE INDEX USING hnsw option `{key}` is not supported"
            )));
        }
    }
    Ok(params)
}

fn claim_index_option(
    seen: &mut std::collections::BTreeSet<&'static str>,
    canonical: &'static str,
    access_method: &str,
    source: &str,
) -> Result<(), SQLError> {
    if !seen.insert(canonical) {
        return Err(SQLError::Unsupported(format!(
            "CREATE INDEX USING {access_method} option `{source}` duplicates `{canonical}`"
        )));
    }
    Ok(())
}

fn parse_positive_usize_option(
    access_method: &str,
    key: &str,
    value: &str,
) -> Result<usize, SQLError> {
    let parsed = value.parse::<usize>().map_err(|_| {
        SQLError::TypeMismatch(format!(
            "CREATE INDEX USING {access_method} option `{key}` must be a positive integer"
        ))
    })?;
    if parsed == 0 {
        return Err(SQLError::TypeMismatch(format!(
            "CREATE INDEX USING {access_method} option `{key}` must be a positive integer"
        )));
    }
    Ok(parsed)
}
