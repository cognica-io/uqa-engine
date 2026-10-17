# CLI Arrow input

`rows.arrow` is an Apache Arrow IPC file containing the non-null value `7` in the nullable `id: int64` field. It was generated independently with PyArrow 26.0.0 and is read by the installed CLI's Arrow foreign handler. The DuckDB check reads an equivalent CSV created by the smoke test; neither check downloads runtime extensions.

Regenerate with Python and PyArrow 26.0.0:

```python
import pyarrow as pa

table = pa.table({"id": pa.array([7], type=pa.int64())})
with pa.OSFile("scripts/fixtures/usql-release/rows.arrow", "wb") as sink:
    with pa.ipc.new_file(sink, table.schema) as writer:
        writer.write_table(table)
```
