//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{lexical, message, ColumnType, IntervalFields, ProductionControl, SQLError, Work};

pub(super) async fn integer<W: Work<Error = SQLError>>(
    value: &str,
    range: std::ops::RangeInclusive<i64>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<i64>, SQLError> {
    lexical::integer(value, range, true, control, work).await
}

pub(super) async fn optional_unsigned<W: Work<Error = SQLError>>(
    value: Option<&str>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<u32>, SQLError> {
    match value {
        None => Ok(None),
        Some(value) => Ok(integer(value, 0..=i64::from(u32::MAX), control, work)
            .await?
            .map(|value| u32::try_from(value).expect("validated unsigned modifier"))),
    }
}

pub(super) async fn temporal<W: Work<Error = SQLError>>(
    value: Option<&str>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<i64>, SQLError> {
    let Some(value) = value else { return Ok(None) };
    let trimmed = lexical::trim(value, control, work).await?;
    let Some(parsed) = integer(trimmed, i64::MIN..=i64::MAX, control, work).await? else {
        return Err(SQLError::TypeMismatch(
            message("invalid temporal precision: ", value, "", control, work).await?,
        ));
    };
    Ok(Some(parsed))
}

pub(super) async fn character_length<W: Work<Error = SQLError>>(
    value: Option<&str>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<u32>, SQLError> {
    let Some(value) = value else { return Ok(None) };
    let Some(length) = optional_unsigned(Some(value), control, work)
        .await?
        .filter(|value| *value > 0)
    else {
        return Err(SQLError::TypeMismatch(
            message(
                "character length must be greater than zero, got ",
                value,
                "",
                control,
                work,
            )
            .await?,
        ));
    };
    Ok(Some(length))
}

async fn part<'a, W: Work<Error = SQLError>>(
    remaining: &mut Option<&'a str>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<&'a str>, SQLError> {
    let Some(text) = remaining.take() else {
        return Ok(None);
    };
    for (index, byte) in text.bytes().enumerate() {
        if index % 1024 == 0 {
            lexical::checkpoint(control, work).await?;
        }
        if byte == b',' {
            *remaining = Some(&text[index + 1..]);
            return Ok(Some(lexical::trim(&text[..index], control, work).await?));
        }
    }
    Ok(Some(lexical::trim(text, control, work).await?))
}

pub(super) async fn numeric<W: Work<Error = SQLError>>(
    modifier: Option<&str>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(Option<u32>, Option<i32>), SQLError> {
    let Some(modifier) = modifier else {
        return Ok((None, None));
    };
    let mut remaining = Some(modifier);
    let first = part(&mut remaining, control, work).await?;
    let precision = optional_unsigned(first, control, work).await?;
    if let Some(precision) = precision {
        let scale = match part(&mut remaining, control, work).await? {
            None => Some(0),
            Some(value) => {
                integer(
                    value,
                    i64::from(i32::MIN)..=i64::from(i32::MAX),
                    control,
                    work,
                )
                .await?
            }
        };
        if let Some(scale) = scale {
            if remaining.is_none() {
                return Ok((
                    Some(precision),
                    Some(i32::try_from(scale).expect("validated numeric scale")),
                ));
            }
        }
    }
    Err(SQLError::TypeMismatch(
        message("invalid numeric modifier `", modifier, "`", control, work).await?,
    ))
}

pub(super) async fn catalog_interval_type<W: Work<Error = SQLError>>(
    modifier: Option<&str>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<ColumnType, SQLError> {
    let Some(modifier) = modifier else {
        return Ok(ColumnType::Interval);
    };
    let mut remaining = Some(modifier);
    let mask = part(&mut remaining, control, work)
        .await?
        .expect("first interval field");
    let mask = integer(mask, i64::MIN..=i64::MAX, control, work)
        .await?
        .and_then(IntervalFields::from_modifier_mask)
        .ok_or_else(|| SQLError::TypeMismatch("invalid interval fields".into()))?;
    let precision = temporal(part(&mut remaining, control, work).await?, control, work).await?;
    if remaining.is_some() {
        return Err(SQLError::TypeMismatch(
            "invalid interval type modifier".into(),
        ));
    }
    ColumnType::with_interval_modifiers(mask, precision)
}
