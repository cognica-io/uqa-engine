#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from hashlib import sha256
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from io import BytesIO
import os
from pathlib import Path
import subprocess
import sys
import tarfile
from tempfile import TemporaryDirectory
from threading import Thread
import unittest


INSTALLER = Path(__file__).resolve().parents[2] / ".github/actions/setup-nextest/install.sh"


@contextmanager
def release_server(payload, *, interrupt=False, ranges=True):
    requests = []

    class Release(BaseHTTPRequestHandler):
        def do_GET(self):
            requested = self.headers.get("Range")
            requests.append(requested)
            offset = int(requested.removeprefix("bytes=").removesuffix("-")) if requested and ranges else 0
            self.send_response(206 if offset else 200)
            if offset:
                self.send_header("Content-Range", f"bytes {offset}-{len(payload) - 1}/{len(payload)}")
            self.send_header("Content-Length", str(len(payload) - offset))
            self.end_headers()
            if interrupt and len(requests) == 1:
                self.wfile.write(payload[:len(payload) // 2])
            else:
                self.wfile.write(payload[offset:])

        def log_message(self, *args):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Release) as http:
        worker = Thread(target=http.serve_forever, kwargs={"poll_interval": 0.02})
        worker.start()
        try:
            yield f"http://127.0.0.1:{http.server_port}/nextest.tar.gz", requests
        finally:
            http.shutdown()
            worker.join()


@unittest.skipUnless(sys.platform in {"linux", "darwin"}, "nextest action runs on Linux and macOS")
class NextestInstallerTest(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        contents = b"#!/bin/sh\nprintf 'nextest fixture\\n'\n"
        exe = tarfile.TarInfo("cargo-nextest")
        exe.size = len(contents)
        exe.mode = 0o755
        data = BytesIO()
        with tarfile.open(fileobj=data, mode="w:gz") as archive:
            archive.addfile(exe, BytesIO(contents))
        self.payload = data.getvalue()
        self.checksum = sha256(self.payload).hexdigest()
        self.cache = self.root / "tools" / "cargo-nextest" / (self.checksum + ".tar.gz")

    def install(self, url, *, job="job", expect_success=True):
        temp = self.root / job
        temp.mkdir()
        path = temp / "github-path"
        env = {**os.environ, "RUNNER_TEMP": str(temp), "RUNNER_TOOL_CACHE": str(self.root / "tools"),
               "GITHUB_PATH": str(path), "no_proxy": "127.0.0.1"}
        result = subprocess.run(["bash", str(INSTALLER), url, self.checksum], env=env,
                                capture_output=True, text=True, timeout=20)
        if expect_success:
            self.assertEqual(result.returncode, 0, result.stderr)
            binary = Path(path.read_text().strip()) / "cargo-nextest"
            self.assertEqual(subprocess.check_output([binary], text=True), "nextest fixture\n")
        else:
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(path.exists())
            self.assertIn("SHA-256", result.stderr)

    def test_interrupted_transfer_resumes_and_a_warm_runner_works_offline(self):
        with release_server(self.payload, interrupt=True) as (url, requests):
            self.install(url)
            self.assertEqual(requests, [None, f"bytes={len(self.payload) // 2}-"])
        self.install(url, job="offline")
        self.assertEqual(self.cache.read_bytes(), self.payload)
        self.assertEqual(list(self.cache.parent.iterdir()), [self.cache])

    def test_server_without_range_support_restarts_the_transfer(self):
        with release_server(self.payload, interrupt=True, ranges=False) as (url, requests):
            self.install(url)
            self.assertEqual(requests, [None, f"bytes={len(self.payload) // 2}-", None])

    def test_corrupt_cache_and_incorrect_download_are_never_installed(self):
        self.cache.parent.mkdir(parents=True)
        self.cache.write_bytes(b"corrupted cache")
        with release_server(b"wrong release") as (url, requests):
            self.install(url, expect_success=False)
            self.assertEqual(requests, [None])
        self.assertEqual(self.cache.read_bytes(), b"corrupted cache")
        with release_server(self.payload) as (url, requests):
            self.install(url, job="restored")
            self.assertEqual(requests, [None])
        self.assertEqual(self.cache.read_bytes(), self.payload)

    def test_concurrent_installers_share_only_the_complete_archive(self):
        with release_server(self.payload) as (url, requests), ThreadPoolExecutor(max_workers=2) as pool:
            installs = [pool.submit(self.install, url, job=job) for job in ("first", "second")]
            for install in installs:
                install.result()
            self.assertIn(requests, ([None], [None, None]))
        self.assertEqual(self.cache.read_bytes(), self.payload)
        self.assertEqual(list(self.cache.parent.iterdir()), [self.cache])
