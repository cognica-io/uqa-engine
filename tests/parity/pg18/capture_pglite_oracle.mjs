// Capture PostgreSQL's text wire results through a separately installed PGlite.
// Usage: node capture_pglite_oracle.mjs /path/to/pglite/dist/index.js input.json output.json
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';

const [modulePath, inputPath, outputPath] = process.argv.slice(2);
const { PGlite } = await import(pathToFileURL(modulePath).href);
const input = await readFile(inputPath, 'utf8');
const packageInfo = JSON.parse(await readFile(new URL('../package.json', pathToFileURL(modulePath))));
const db = await PGlite.create();
const version = (await db.query('SELECT version()')).rows[0].version;
if (!version.startsWith('PostgreSQL 18.')) throw new Error(`unexpected reference: ${version}`);

function response(raw) {
  const data = Buffer.from(raw);
  const result = { command_tags: [], results: [], error: null, notices: [] };
  let current = { columns: [], type_oids: [], rows: [] };
  for (let position = 0; position < data.length;) {
    const kind = String.fromCharCode(data[position]);
    const end = position + 1 + data.readInt32BE(position + 1);
    let cursor = position + 5;
    const string = () => {
      const stop = data.indexOf(0, cursor);
      const value = data.toString('utf8', cursor, stop);
      cursor = stop + 1;
      return value;
    };
    if (kind === 'T') {
      const count = data.readUInt16BE(cursor); cursor += 2;
      current = { columns: [], type_oids: [], rows: [] };
      for (let i = 0; i < count; i++) {
        current.columns.push(string());
        current.type_oids.push(data.readUInt32BE(cursor + 6));
        cursor += 18;
      }
    } else if (kind === 'D') {
      const count = data.readUInt16BE(cursor); cursor += 2;
      const row = [];
      for (let i = 0; i < count; i++) {
        const length = data.readInt32BE(cursor); cursor += 4;
        row.push(length === -1 ? null : data.toString('utf8', cursor, cursor + length));
        if (length !== -1) cursor += length;
      }
      current.rows.push(row);
    } else if (kind === 'C') {
      result.command_tags.push(string());
      result.results.push(current);
      current = { columns: [], type_oids: [], rows: [] };
    } else if (kind === 'E' || kind === 'N') {
      const fields = {};
      while (data[cursor] !== 0) {
        const key = String.fromCharCode(data[cursor++]);
        fields[key] = string();
      }
      const diagnostic = { sqlstate: fields.C, message: fields.M, detail: fields.D ?? null, hint: fields.H ?? null };
      if (kind === 'E') result.error = diagnostic;
      else result.notices.push({ severity: fields.V ?? fields.S, ...diagnostic });
    }
    position = end;
  }
  return result;
}

const cases = [];
for (const item of JSON.parse(input)) {
  const query = Buffer.from(item.sql + '\0');
  const message = Buffer.alloc(query.length + 5);
  message[0] = 81;
  message.writeInt32BE(query.length + 4, 1);
  query.copy(message, 5);
  cases.push({ ...item, ...response(await db.execProtocolRaw(message)) });
}
await db.close();
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const provenance = { reference: 'PostgreSQL in PGlite WASM; SQL semantics only, not concurrency or performance', package: `@electric-sql/pglite@${packageInfo.version}`, capture_script: 'tests/parity/pg18/capture_pglite_oracle.mjs', capture_script_sha256: hash(await readFile(new URL(import.meta.url))), input_sha256: hash(input) };
await writeFile(outputPath, '{\n' + `"postgresql_version":${JSON.stringify(version)},\n"provenance":${JSON.stringify(provenance)},\n"cases":[\n` + cases.map(item => JSON.stringify(item)).join(',\n') + '\n]}\n');
console.log(`Captured ${cases.length} cases from ${version}`);
