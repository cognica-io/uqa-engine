//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Host-neutral SQL assertions shared by native and real-browser drivers.
export async function checkBindingStep(engine, fixture, step, assertEqual) {
  if (step.error_contains) {
    try {
      await engine.sql(step.sql, step.params ?? []);
    } catch (error) {
      assertEqual(error.message.includes(step.error_contains), true, `${step.name}: ${error.message}`);
      return;
    }
    throw new Error(`${step.name}: expected an error containing ${step.error_contains}`);
  }
  const result = await engine.sql(step.sql, step.params ?? []);
  if (step.rows_ref || step.rows) {
    assertEqual(result.rows, step.rows_ref ? fixture[step.rows_ref] : step.rows, step.name);
  }
}
