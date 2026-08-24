import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { createRequire } from "node:module";

import pg from "pg";
import { from as copyFrom, to as copyTo } from "pg-copy-streams";

const require = createRequire(import.meta.url);
const { Pool, types } = pg;

types.setTypeParser(23, "binary", (value) => {
  if (value.length !== 4) throw new Error(`invalid binary int4 length: ${value.length}`);
  return (
    (value.charCodeAt(0) << 24)
    | (value.charCodeAt(1) << 16)
    | (value.charCodeAt(2) << 8)
    | value.charCodeAt(3)
  );
});

function requireSqlstate(error, expected) {
  if (error.code !== expected) {
    throw new Error(`expected SQLSTATE ${expected}, got ${error.code}: ${error}`);
  }
}

const pool = new Pool({
  connectionString: process.env.UQA_PG18_MATRIX_DSN,
  max: 1,
  idleTimeoutMillis: 30_000,
});

const client = await pool.connect();
try {
  const prepared = {
    name: "matrix-add",
    text: "SELECT $1::int4 + 1 AS value",
    values: [41],
    binary: true,
    rowMode: "array",
  };
  let result = await client.query(prepared);
  if (result.rows[0][0] !== 42) throw new Error(`unexpected prepared result: ${result.rows}`);
  prepared.values = [99];
  result = await client.query(prepared);
  if (result.rows[0][0] !== 100) throw new Error(`unexpected prepared reuse: ${result.rows}`);

  await client.query("BEGIN");
  try {
    await client.query("SELECT 1 / 0");
    throw new Error("division by zero unexpectedly succeeded");
  } catch (error) {
    requireSqlstate(error, "22012");
  }
  try {
    await client.query("SELECT 1");
    throw new Error("failed transaction unexpectedly accepted a query");
  } catch (error) {
    requireSqlstate(error, "25P02");
  }
  await client.query("ROLLBACK");

  await client.query("CREATE TEMP TABLE matrix_copy (id int4, value text)");
  await pipeline(
    Readable.from(["1\tone\n", "2\ttwo\n"]),
    client.query(copyFrom("COPY matrix_copy FROM STDIN")),
  );
  result = await client.query("SELECT count(*)::int8 FROM matrix_copy");
  if (result.rows[0].count !== "2") throw new Error(`unexpected copy count: ${result.rows}`);
  const output = client.query(copyTo("COPY matrix_copy TO STDOUT"));
  const chunks = [];
  for await (const chunk of output) chunks.push(chunk);
  if (Buffer.concat(chunks).toString("utf8") !== "1\tone\n2\ttwo\n") {
    throw new Error(`unexpected COPY output: ${Buffer.concat(chunks)}`);
  }
} finally {
  client.release();
}

const reused = await pool.query("SELECT 1");
if (reused.rows[0]["?column?"] !== 1) throw new Error(`unexpected pooled result: ${reused.rows}`);
await pool.end();

console.log(JSON.stringify({
  driver: "node-postgres",
  pg: require("pg/package.json").version,
  pgCopyStreams: require("pg-copy-streams/package.json").version,
  operations: [
    "binary-result",
    "prepared-reuse",
    "copy-in-out",
    "transaction-error-recovery",
    "pool-reuse",
  ],
}));
