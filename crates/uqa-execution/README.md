# uqa-execution

`uqa-execution` provides query, mutation, schema and retrieval execution, with physical rows, batches, joins, sorting and spill. Its retrieval driver consumes catalog, index, graph and model inputs; Engine binds the active session and transaction.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
