//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { checkBindingStep } from "./bindings.core.mjs";

// SQL values come from PostgreSQL; row containers follow each binding's host contract.
export async function runBitStringBinding(open, path, fixture, assertEqual) {
  assertEqual(fixture.schema_version, 1);
  let engine = await open(path);
  const check = (name, sql, rows_ref) => checkBindingStep(engine, fixture, { name, sql, rows_ref }, assertEqual);
  try {
    await check("bit literal lengths and containers", fixture.literal_query, "literal_rows");
    for (const sql of fixture.setup) await engine.sql(sql);
    await check("stored bit fields and containers", fixture.query, "initial_rows");
    await engine.sql(fixture.parameter_sql, fixture.params);
    await check("text parameters with explicit bit casts", fixture.query, "rows");
    await engine.sql(fixture.index_sql);
    await check("bit index lookup", fixture.probe_sql, "probe_rows");
    await checkBindingStep(engine, fixture, {
      name: "invalid bit assignment",
      sql: fixture.invalid_sql,
      error_contains: fixture.error_contains,
    }, assertEqual);
    await engine.sql(fixture.rollback_sql);
    await check("bit rollback", fixture.query, "rows");
    await engine.close();
    engine = null;
    engine = await open(path);
    await check("reopened bit values and containers", fixture.query, "rows");
    await check("reopened bit index", fixture.probe_sql, "probe_rows");
  } finally {
    await engine?.close();
  }
}
