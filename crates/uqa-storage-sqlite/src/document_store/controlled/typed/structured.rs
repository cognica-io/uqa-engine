//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compound typed payloads retain child buffers and reuse Core's array and temporal representations.

use super::{
    array, decode, finish, insert, literal, modern, string, BTreeMap, Budgeted, BudgetedVec,
    Container, JsonReadError, Kind, StorageReadControl, Value,
};

pub(super) fn map(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
    buffered: bool,
) -> Result<Budgeted<Value>, JsonReadError> {
    let mut container = Container::new(input, control, depth)?;
    if container.kind != Kind::Object {
        return Err(JsonReadError::InvalidJson);
    }
    let mut fields = BTreeMap::new();
    let mut memory = control.memory().empty_reservation();
    while let Some(item) = container.next()? {
        let name = string(item.key.expect("map key"), control)?;
        let value = decode(item.value, control, depth - 1, buffered)?;
        insert(&mut fields, &mut memory, name, value)?;
    }
    finish(Value::Map(fields), memory, control)
}

pub(super) fn record(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
    buffered: bool,
) -> Result<Budgeted<Value>, JsonReadError> {
    let mut container = array(input, control, depth)?;
    let mut fields = BudgetedVec::new(control.memory());
    let mut memory = control.memory().empty_reservation();
    while let Some(item) = container.next()? {
        let mut pair = array(item.value, control, depth - 1)?;
        let name = string(
            pair.next()?.ok_or(JsonReadError::InvalidJson)?.value,
            control,
        )?;
        let value = decode(
            pair.next()?.ok_or(JsonReadError::InvalidJson)?.value,
            control,
            depth - 2,
            buffered,
        )?;
        if pair.next()?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
        fields.reserve(1)?;
        let (name, name_memory) = name.into_parts();
        let (value, value_memory) = value.into_parts();
        memory.absorb(name_memory);
        memory.absorb(value_memory);
        fields.push((name, value))?;
    }
    let (fields, retained) = fields.into_parts();
    memory.absorb(retained);
    Ok(Budgeted::new(Value::Record(fields), memory))
}

pub(super) fn sql_array(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
    buffered: bool,
) -> Result<Budgeted<Value>, JsonReadError> {
    let mut container = Container::new(input, control, depth)?;
    let (mut elements, mut bounds) = (None, None);
    if container.kind == Kind::Array {
        elements = Some(container.next()?.ok_or(JsonReadError::InvalidJson)?.value);
        bounds = Some(container.next()?.ok_or(JsonReadError::InvalidJson)?.value);
        if container.next()?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
    } else {
        while let Some(item) = container.next()? {
            let name = string(item.key.expect("array field"), control)?;
            let slot = match name.as_str() {
                "elements" => &mut elements,
                "lower_bounds" => &mut bounds,
                _ => continue,
            };
            if slot.replace(item.value).is_some() {
                return Err(JsonReadError::InvalidJson);
            }
        }
    }
    let elements = modern(
        elements.ok_or(JsonReadError::InvalidJson)?,
        control,
        depth - 1,
    )?;
    let bounds = lower_bounds(
        bounds.ok_or(JsonReadError::InvalidJson)?,
        control,
        depth - 1,
        buffered,
    )?;
    if !matches!(&*elements, Value::List(_)) {
        return Err(JsonReadError::InvalidJson);
    }
    let mut fields = BTreeMap::new();
    let mut memory = control.memory().empty_reservation();
    let (tag, tag_memory) = literal("array", control)?.into_parts();
    insert(
        &mut fields,
        &mut memory,
        literal("$uqa_type", control)?,
        Budgeted::new(Value::Str(tag), tag_memory),
    )?;
    insert(
        &mut fields,
        &mut memory,
        literal("values", control)?,
        elements,
    )?;
    insert(
        &mut fields,
        &mut memory,
        literal("lower_bounds", control)?,
        bounds,
    )?;
    let result =
        Value::from_json_fields_budgeted(Budgeted::new(fields, memory), control.cancellation())?;
    if !matches!(&*result, Value::Array(_)) {
        return Err(JsonReadError::InvalidJson);
    }
    Ok(result)
}

pub(super) fn temporal(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
) -> Result<Budgeted<Value>, JsonReadError> {
    let mut container = Container::new(input, control, depth)?;
    let mut fields = BTreeMap::new();
    let mut memory = control.memory().empty_reservation();
    if container.kind == Kind::Array {
        let tag = string(
            container.next()?.ok_or(JsonReadError::InvalidJson)?.value,
            control,
        )?;
        let names: &[&str] = match tag.as_str() {
            "date" => &["days"],
            "time" | "timestamp" | "timestamp_tz" => &["micros"],
            "time_tz" => &["micros", "offset_minutes"],
            "interval" => &["months", "days", "micros"],
            _ => return Err(JsonReadError::InvalidJson),
        };
        let (tag, retained) = tag.into_parts();
        insert(
            &mut fields,
            &mut memory,
            literal("$uqa_type", control)?,
            Budgeted::new(Value::Str(tag), retained),
        )?;
        for name in names {
            let value = temporal_number(
                name,
                container.next()?.ok_or(JsonReadError::InvalidJson)?.value,
                control,
            )?;
            insert(&mut fields, &mut memory, literal(name, control)?, value)?;
        }
        if container.next()?.is_some() {
            return Err(JsonReadError::InvalidJson);
        }
    } else {
        while let Some(item) = container.next()? {
            let name = string(item.key.expect("temporal field"), control)?;
            if !matches!(
                name.as_str(),
                "$uqa_type" | "days" | "micros" | "offset_minutes" | "months"
            ) || fields.contains_key(name.as_str())
            {
                return Err(JsonReadError::InvalidJson);
            }
            let value = if name.as_str() == "$uqa_type" {
                let (value, memory) = string(item.value, control)?.into_parts();
                Budgeted::new(Value::Str(value), memory)
            } else {
                temporal_number(&name, item.value, control)?
            };
            insert(&mut fields, &mut memory, name, value)?;
        }
    }
    let required: &[&str] = match fields.get("$uqa_type") {
        Some(Value::Str(kind)) => match kind.as_str() {
            "date" => &["$uqa_type", "days"],
            "time" | "timestamp" | "timestamp_tz" => &["$uqa_type", "micros"],
            "time_tz" => &["$uqa_type", "micros", "offset_minutes"],
            "interval" => &["$uqa_type", "months", "days", "micros"],
            _ => return Err(JsonReadError::InvalidJson),
        },
        _ => return Err(JsonReadError::InvalidJson),
    };
    if fields.len() != required.len() || required.iter().any(|name| !fields.contains_key(*name)) {
        return Err(JsonReadError::InvalidJson);
    }
    let result =
        Value::from_json_fields_budgeted(Budgeted::new(fields, memory), control.cancellation())?;
    if !matches!(&*result, Value::Temporal(_)) {
        return Err(JsonReadError::InvalidJson);
    }
    Ok(result)
}

fn lower_bounds(
    input: &[u8],
    control: &StorageReadControl,
    depth: usize,
    buffered: bool,
) -> Result<Budgeted<Value>, JsonReadError> {
    let mut container = array(input, control, depth)?;
    let mut values = BudgetedVec::new(control.memory());
    while let Some(item) = container.next()? {
        let value = i32::try_from(super::integer(item.value, control, buffered)?)
            .map_err(|_| JsonReadError::InvalidJson)?;
        values.push(Value::Int(i64::from(value)))?;
    }
    let (values, memory) = values.into_parts();
    Ok(Budgeted::new(Value::List(values), memory))
}

fn temporal_number(
    field: &str,
    input: &[u8],
    control: &StorageReadControl,
) -> Result<Budgeted<Value>, JsonReadError> {
    let value = super::integer(input, control, true)?;
    if field != "micros" {
        i32::try_from(value).map_err(|_| JsonReadError::InvalidJson)?;
    }
    Ok(Budgeted::new(
        Value::Int(value),
        control.memory().empty_reservation(),
    ))
}
