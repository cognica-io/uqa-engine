#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise the installed native HttpEngine exception and stream conversion."""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
import uqa


@pytest.fixture
def diagnostic_origin():
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            assert self.headers["Authorization"] == "Bearer diagnostic-test"
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            diagnostic = {"sqlstate": "42703", "category": "undefined_column", "statement_index": 1, "position": 8}
            if request.get("sql") == "malformed":
                diagnostic["category"] = "private SQL value"
            detail = {"code": "SQL_EXECUTION_FAILED", "message": "private SQL value", "diagnostic": diagnostic}
            streaming = self.path == "/v1/sql/stream"
            payload = {"type": "error", **detail, "request_id": "qry_diagnostic"} if streaming else {"error": detail, "request_id": "qry_diagnostic"}
            body = (json.dumps(payload) + ("\n" if streaming else "")).encode()
            self.send_response(200 if streaming else 400)
            self.send_header("Content-Type", "application/x-ndjson" if streaming else "application/json")
            self.send_header("X-Request-Id", "qry_diagnostic")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


def test_sql_batch_and_stream_preserve_bounded_diagnostic(diagnostic_origin):
    engine = uqa.HttpEngine(diagnostic_origin, "diagnostic-test")
    expected = {"sqlstate": "42703", "category": "undefined_column", "statement_index": 1, "position": 8}
    for request in [lambda: engine.sql("SELECT 1"), lambda: engine.sql_batch([("SELECT 1", [])])]:
        with pytest.raises(uqa.HttpEngineError) as raised:
            request()
        error = raised.value
        assert isinstance(error, RuntimeError)
        assert error.diagnostic == expected
        assert error.code == "SQL_EXECUTION_FAILED"
        assert error.status == 400
        assert error.request_id == "qry_diagnostic"
        assert "batch statement 2" in str(error)
        assert "private SQL value" not in repr(error)
    frames = list(engine.sql_stream("SELECT 1"))
    assert len(frames) == 1
    assert frames[0]["type"] == "error"
    assert frames[0]["diagnostic"] == expected


def test_malformed_optional_diagnostic_preserves_original_error(diagnostic_origin):
    engine = uqa.HttpEngine(diagnostic_origin, "diagnostic-test")
    with pytest.raises(uqa.HttpEngineError) as raised:
        engine.sql("malformed")
    assert raised.value.code == "SQL_EXECUTION_FAILED"
    assert raised.value.diagnostic is None
    assert "private SQL value" not in repr(raised.value)
    frames = list(engine.sql_stream("malformed"))
    assert len(frames) == 1
    assert frames[0]["code"] == "SQL_EXECUTION_FAILED"
    assert "diagnostic" not in frames[0]
