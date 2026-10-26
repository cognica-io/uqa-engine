#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.
"""Package, verify and exercise the standalone usql release assets."""

from __future__ import annotations

import argparse
import contextlib
import functools
import hashlib
import http.server
import importlib.util
import io
import json
import os
import pathlib
import re
import subprocess
import tarfile
import tempfile
import threading
import time
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("release_licenses", ROOT / "scripts/check-release-licenses.py")
LICENSES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LICENSES)
TARGETS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
)
INSTALLERS = ("install-usql.sh", "install-usql.ps1")
FEATURES = ["arrow", "duckdb", "kuromoji", "nori"]


def version() -> str:
    matches = re.findall(r'^version = "([0-9]+\.[0-9]+\.[0-9]+)"$', (ROOT / "Cargo.toml").read_text(), re.M)
    if len(matches) != 1:
        raise ValueError("expected one workspace release version")
    return matches[0]


def executable(target: str) -> str:
    return "usql.exe" if "windows" in target else "usql"


def asset_name(target: str) -> str:
    if target not in TARGETS:
        raise ValueError(f"unsupported release target: {target}")
    suffix = ".zip" if "windows" in target else ".tgz"
    # Match cargo-binstall's crate/version/target discovery convention.
    return f"uqa-cli-{version()}-{target}{suffix}"


def checksum(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"{digest.hexdigest()}  {path.name}\n"


def installer(name: str) -> bytes:
    return (ROOT / "scripts" / name).read_bytes().replace(b"@UQA_VERSION@", version().encode())


def metadata(target: str) -> dict[str, bytes]:
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    return {
        **LICENSES.canonical_payloads(), **LICENSES.binding_dictionary_payloads(),
        **{f"THIRD-PARTY/{path.name}": path.read_bytes()
           for path in sorted((ROOT / "scripts/usql-release/licenses").glob("*.txt"))},
        "LICENSE-NOTICE.md": (ROOT / "crates/uqa-cli/LICENSE-NOTICE.md").read_bytes(),
        "usql-release.json": (json.dumps({"version": version(), "target": target,
                                        "commit": commit, "features": FEATURES}, sort_keys=True) + "\n").encode(),
    }


def package(target: str, binary: pathlib.Path, output: pathlib.Path) -> pathlib.Path:
    output.mkdir(parents=True, exist_ok=True)
    archive = output / asset_name(target)
    files = {executable(target): binary.read_bytes(), **metadata(target)}
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as writer:
            for name, data in files.items():
                writer.writestr(name, data)
    else:
        with tarfile.open(archive, "w:gz") as writer:
            for name, data in files.items():
                info = tarfile.TarInfo(name)
                info.size = len(data)
                info.mode = 0o755 if name == executable(target) else 0o644
                writer.addfile(info, io.BytesIO(data))
    archive.with_name(archive.name + ".sha256").write_text(checksum(archive), encoding="utf-8")
    for name in INSTALLERS:
        (output / name).write_bytes(installer(name))
    verify(target, output)
    return archive


def verify(target: str, directory: pathlib.Path) -> None:
    archive = directory / asset_name(target)
    if archive.with_name(archive.name + ".sha256").read_text() != checksum(archive):
        raise ValueError(f"checksum mismatch: {archive}")
    for name in INSTALLERS:
        if (directory / name).read_bytes() != installer(name):
            raise ValueError(f"installer differs from release source: {name}")
    expected = metadata(target)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as reader:
            names = reader.namelist()
            files = {name: reader.read(name) for name in names}
    else:
        with tarfile.open(archive) as reader:
            entries = reader.getmembers()
            if any(not entry.isfile() for entry in entries):
                raise ValueError("archive must contain only regular files")
            names = [entry.name for entry in entries]
            files = {entry.name: reader.extractfile(entry).read() for entry in entries}
    if len(names) != len(set(names)) or set(names) != {*expected, executable(target)}:
        raise ValueError(f"unexpected archive inventory: {archive}")
    if any(files[name] != data for name, data in expected.items()):
        raise ValueError(f"archive metadata differs from release source: {archive}")
    LICENSES.check_archive(archive, LICENSES.canonical_payloads(), require_nori=True, require_kuromoji=True)


def install_binstall(tool: str, target: str, directory: pathlib.Path, destination: pathlib.Path) -> None:
    """Exercise the release download/extraction before its public URL exists."""
    class DownloadHandler(http.server.SimpleHTTPRequestHandler):
        def handle(self) -> None:
            try:
                super().handle()
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                # Availability probes may close without downloading the body.
                pass

        def log_message(self, format: str, *args: object) -> None:
            pass

    handler = functools.partial(DownloadHandler, directory=str(directory))
    with tempfile.TemporaryDirectory(prefix="usql-binstall-manifest-") as temporary, \
            http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            # Binstall resolves repository redirects before its package URL.
            # Relocate both transport origins while retaining archive discovery.
            origin = f"http://127.0.0.1:{server.server_port}"
            manifest = pathlib.Path(temporary) / "Cargo.toml"
            local_binstall_manifest(manifest, origin)
            url = origin + "/{ name }-{ version }-{ target }{ archive-suffix }"
            env = {key: value for key, value in os.environ.items() if key not in ("GH_TOKEN", "GITHUB_TOKEN")}
            subprocess.run([tool, "--manifest-path", str(manifest),
                            "--targets", target, "--pkg-url", url, "--allow-insecure-http",
                            "--strategies", "crate-meta-data", "--no-discover-github-token",
                            "--root", str(destination.parent / "cargo-root"),
                            "--settings", str(destination.parent / "binstall.toml"),
                            "--no-confirm", "--no-track", "--install-path", str(destination), "uqa-cli"],
                           env=env, check=True)
        finally:
            server.shutdown()
            thread.join()


def local_binstall_manifest(destination: pathlib.Path, repository: str) -> None:
    """Keep the release manifest and workspace inheritance with a local origin."""
    source = (ROOT / "crates/uqa-cli/Cargo.toml").read_text(encoding="utf-8")
    inherited = "repository.workspace = true"
    if source.count(inherited) != 1:
        raise ValueError("expected one inherited CLI repository")
    destination.write_text(source.replace(
        inherited, f"repository = {json.dumps(repository)}\nworkspace = {json.dumps(str(ROOT))}"),
        encoding="utf-8")


@contextlib.contextmanager
def temporary_installation():
    temporary = tempfile.TemporaryDirectory(prefix="usql-installed-")
    try:
        yield pathlib.Path(temporary.name)
    finally:
        # Windows can retain an executable image after its process has exited.
        # Wait only for a reported sharing violation; permanent locks still fail.
        deadline = time.monotonic() + 5.0
        while True:
            try:
                temporary.cleanup()
                break
            except OSError as error:
                remaining = deadline - time.monotonic()
                if getattr(error, "winerror", None) != 32 or remaining <= 0:
                    raise
                time.sleep(min(0.05, remaining))


def smoke(target: str, directory: pathlib.Path, binstall: str | None = None) -> None:
    verify(target, directory)
    directory = directory.resolve()
    with temporary_installation() as work:
        destination = work / "installed bin"
        if "windows" in target:
            command = ["pwsh", "-NoProfile", "-File", str(directory / "install-usql.ps1"),
                       "-ArchiveDirectory", str(directory), "-BinDir", str(destination)]
        else:
            command = ["sh", str(directory / "install-usql.sh"), "--archive-dir", str(directory),
                       "--bin-dir", str(destination)]
        subprocess.run(command, check=True)
        binary = destination / executable(target)
        if binstall:
            cargo_destination = work / "cargo bin"
            install_binstall(binstall, target, directory, cargo_destination)
            if (cargo_destination / executable(target)).read_bytes() != binary.read_bytes():
                raise ValueError("cargo-binstall did not install the exact release executable")

        def query(sql: str) -> str:
            result = subprocess.run([str(binary), "--db", str(work / "smoke.uqa"), "--copy-text", "-c", sql],
                                    cwd=work, input="", encoding="utf-8", capture_output=True)
            if result.returncode:
                raise RuntimeError(f"installed CLI failed for {sql!r}: {result.stdout}{result.stderr}")
            return result.stdout.strip()

        if query("SELECT 1") != "1":
            raise ValueError("installed CLI did not execute SQL")
        query("CREATE TABLE kept(id INTEGER PRIMARY KEY); INSERT INTO kept VALUES (7)")
        if query("SELECT id FROM kept") != "7":
            raise ValueError("installed CLI did not reopen stored data")
        for analyzer, text, index, expected in (("nori", "한국 경제", 0, "한국"), ("kuromoji", "ＵＱＡで走りました", 1, "走る")):
            if query(f"SELECT analysis -> 'tokens' -> {index} ->> 'term' FROM analyze_text('{analyzer}', '{text}')") != expected:
                raise ValueError(f"installed CLI does not include {analyzer}")
        (work / "rows.csv").write_text("id\n7\n", encoding="utf-8")
        (work / "rows.arrow").write_bytes((ROOT / "scripts/fixtures/usql-release/rows.arrow").read_bytes())
        for handler, source in (("duckdb", "rows.csv"), ("arrow", "rows.arrow")):
            query(f"CREATE SERVER {handler}_source FOREIGN DATA WRAPPER {handler}_fdw; "
                  f"CREATE FOREIGN TABLE {handler}_rows(id BIGINT) SERVER {handler}_source OPTIONS (source '{source}')")
            if query(f"SELECT id FROM {handler}_rows") != "7":
                raise ValueError(f"installed CLI cannot read the {handler} source")
        invalid = subprocess.run([str(binary), "-c", "SELECT * FROM missing_release_table"],
                                 cwd=work, input="", text=True, capture_output=True)
        if invalid.returncode == 0:
            raise ValueError("installed CLI hides SQL failure")
    print(f"Installed CLI SQL, persistence, analyzers and foreign handlers passed: {target}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("package", "verify", "smoke"))
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--directory", type=pathlib.Path, required=True)
    parser.add_argument("--binary", type=pathlib.Path)
    parser.add_argument("--binstall", help="cargo-binstall executable to verify during smoke")
    args = parser.parse_args()
    if args.command != "verify" and not args.target:
        parser.error("package and smoke require --target")
    if args.command == "package":
        binary = args.binary or ROOT / "target" / args.target / "release-stripped" / executable(args.target)
        print(package(args.target, binary, args.directory))
    elif args.command == "smoke":
        smoke(args.target, args.directory, args.binstall)
    else:
        for target in (args.target,) if args.target else TARGETS:
            verify(target, args.directory)


if __name__ == "__main__":
    main()
