//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Host-neutral schedules for the native addon and browser WASM artifacts.
export const concurrentIsolationLevels = [
  "READ COMMITTED",
  "READ UNCOMMITTED",
  "REPEATABLE READ",
  "SERIALIZABLE",
];

async function rows(engine, sql) {
  return (await engine.sql(sql)).rows.map(({ id, value }) => `${id}|${value}`);
}

async function statements(engine, sql, isolation) {
  for (const statement of sql) {
    await engine.sql(statement === "BEGIN" ? `BEGIN ISOLATION LEVEL ${isolation}` : statement);
  }
}

export async function runConcurrentWriterCase(open, path, oracle, schedule, isolation, assertEqual) {
  const sessions = [];
  try {
    const first = await open(path);
    sessions.push(first);
    await statements(first, oracle.setup, isolation);
    const second = await first.newSession();
    sessions.push(second);
    const observer = await first.newSession();
    sessions.push(observer);
    await statements(first, schedule.a_before, isolation);
    assertEqual(await rows(first, oracle.observe), ["1|10", "2|0"], "first private write");
    assertEqual(await rows(observer, oracle.observe), ["1|0", "2|0"], "uncommitted write is private");

    // No first-session completion is sent until the second session commits.
    await statements(second, schedule.b, isolation);
    assertEqual(await rows(second, oracle.observe), schedule.before_a_end, "second committed independently");
    assertEqual(await rows(observer, oracle.observe), schedule.before_a_end, "peer commit is published");
    const peerValue = isolation === "READ COMMITTED" || isolation === "READ UNCOMMITTED" ? 20 : 0;
    assertEqual(await rows(first, oracle.observe), ["1|10", `2|${peerValue}`], "original isolation and private write");

    for (const statement of schedule.a_finish) {
      await first.sql(statement);
      if (statement.startsWith("ROLLBACK TO ")) {
        assertEqual(await rows(first, oracle.observe), [schedule.after_a_end[0], `2|${peerValue}`], "savepoint undo preserves the original snapshot");
        assertEqual(await rows(observer, oracle.observe), schedule.before_a_end, "savepoint undo remains private");
      }
    }
    for (const session of sessions) {
      assertEqual(await rows(session, oracle.observe), schedule.after_a_end, "completed transaction visibility");
    }
  } finally {
    for (const session of sessions.reverse()) await session.close();
  }

  const reopened = await open(path);
  try {
    assertEqual(await rows(reopened, oracle.observe), schedule.after_a_end, "closed database reopen");
  } finally {
    await reopened.close();
  }
}
