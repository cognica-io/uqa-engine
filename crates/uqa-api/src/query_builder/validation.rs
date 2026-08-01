use super::SQLError;

pub(super) fn validate_retrieval_signals(
    name: &str,
    signals: &[&str],
    minimum: usize,
) -> Result<(), SQLError> {
    if signals.len() < minimum {
        return Err(SQLError::BadArity {
            name: name.to_string(),
            expected: format!(">={minimum} signals"),
            actual: signals.len(),
        });
    }
    if signals.iter().any(|signal| signal.trim().is_empty()) {
        return Err(SQLError::TypeMismatch(format!(
            "{name} signal expressions cannot be empty"
        )));
    }
    Ok(())
}

pub(super) fn validate_stage_count(count: usize) -> Result<(), SQLError> {
    if count == 0 {
        return Err(SQLError::BadArity {
            name: "staged_retrieval".into(),
            expected: ">=1 stage".into(),
            actual: 0,
        });
    }
    Ok(())
}

pub(super) fn validate_stage_cutoffs(
    cutoffs: impl IntoIterator<Item = usize>,
) -> Result<(), SQLError> {
    for cutoff in cutoffs {
        validate_positive_sql_usize("staged_retrieval top_k", cutoff)?;
    }
    Ok(())
}

pub(super) fn validate_vector_query(
    name: &str,
    field: &str,
    vector: &[f32],
    k: usize,
) -> Result<(), SQLError> {
    validate_field_name(name, field)?;
    if vector.is_empty() || vector.iter().any(|component| !component.is_finite()) {
        return Err(SQLError::TypeMismatch(format!(
            "{name} requires a non-empty finite query vector"
        )));
    }
    validate_positive_sql_usize(&format!("{name} k"), k)
}

pub(super) fn validate_field_name(name: &str, field: &str) -> Result<(), SQLError> {
    if field.trim().is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "{name} field name cannot be empty"
        )));
    }
    Ok(())
}

fn validate_positive_sql_usize(label: &str, value: usize) -> Result<(), SQLError> {
    if value == 0 || i64::try_from(value).is_err() {
        return Err(SQLError::TypeMismatch(format!(
            "{label} must be positive and fit in a SQL BIGINT, got {value}"
        )));
    }
    Ok(())
}

pub(super) fn validate_fusion_alpha(name: &str, alpha: f64) -> Result<(), SQLError> {
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        return Err(SQLError::TypeMismatch(format!(
            "{name} alpha must be finite and in [0, 1], got {alpha:?}"
        )));
    }
    Ok(())
}

pub(super) fn validate_probability_threshold(name: &str, threshold: f64) -> Result<(), SQLError> {
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(SQLError::TypeMismatch(format!(
            "{name} threshold must be finite and in [0, 1], got {threshold:?}"
        )));
    }
    Ok(())
}

pub(super) fn render_vector(query: &[f32]) -> String {
    query
        .iter()
        .map(f32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
