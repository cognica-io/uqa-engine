//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::SQLError;

/// Raw options; Storage resolves defaults and validates them against the target dimension.
#[derive(Debug, Default, PartialEq)]
pub struct DiskANNIndexOptions {
    pub max_degree: Option<usize>,
    pub build_list_size: Option<usize>,
    pub search_list_size: Option<usize>,
    pub alpha: Option<f64>,
    pub beam_width: Option<usize>,
    pub pq_bytes: Option<usize>,
    pub seed: Option<u64>,
}

pub fn parse_diskann_index_options(
    options: &[(String, String)],
) -> Result<DiskANNIndexOptions, SQLError> {
    let mut params = DiskANNIndexOptions::default();
    let mut seen = std::collections::BTreeSet::new();
    for (key, value) in options {
        let canonical = match key.to_ascii_lowercase().as_str() {
            "max_degree" => "max_degree",
            "build_list_size" => "build_list_size",
            "search_list_size" => "search_list_size",
            "alpha" => "alpha",
            "beam_width" => "beam_width",
            "pq_bytes" => "pq_bytes",
            "seed" => "seed",
            _ => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX USING diskann option `{key}` is not supported"
                )));
            }
        };
        super::claim_index_option(&mut seen, canonical, "diskann", key)?;
        if canonical == "alpha" {
            let parsed = value.parse::<f64>().ok().filter(|value| value.is_finite());
            params.alpha = Some(parsed.ok_or_else(|| {
                SQLError::TypeMismatch(format!(
                    "CREATE INDEX USING diskann option `{key}` must be a finite real number"
                ))
            })?);
        } else if canonical == "seed" {
            params.seed = Some(value.parse::<u64>().map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "CREATE INDEX USING diskann option `{key}` must be an unsigned integer"
                ))
            })?);
        } else {
            let target = match canonical {
                "max_degree" => &mut params.max_degree,
                "build_list_size" => &mut params.build_list_size,
                "search_list_size" => &mut params.search_list_size,
                "beam_width" => &mut params.beam_width,
                "pq_bytes" => &mut params.pq_bytes,
                _ => unreachable!(),
            };
            *target = Some(super::parse_positive_usize_option("diskann", key, value)?);
        }
    }
    Ok(params)
}

#[cfg(test)]
mod tests;
