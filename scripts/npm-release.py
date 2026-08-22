#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Build and verify the multi-platform npm release inventory."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass


ROOT = pathlib.Path(__file__).resolve().parents[1]
NODE_ROOT = ROOT / "crates" / "uqa-node"
WASM_ROOT = ROOT / "crates" / "uqa-wasm" / "js"
ROOT_PACKAGE = "@cognica-io/uqa"
WASM_PACKAGE = "@cognica-io/uqa-wasm"
PLATFORM_PACKAGE_BASE = "@cognica-io/uqa"
NPM_REGISTRY = "https://registry.npmjs.org/"
PUBLISH_CONFIG = {"registry": NPM_REGISTRY, "access": "public"}
LEGAL_FILES = (
    "LICENSE-NOTICE.md",
    "LICENSE",
    "LICENSING.md",
    "LICENSES/UQA-FOSS-EXCEPTION-1.0.txt",
    "LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt",
)
ROOT_PACKAGE_FILES = ("README.md", "api.js", "index.js", "index.d.ts", *LEGAL_FILES)


class ReleaseError(RuntimeError):
    """A release archive or source contract is invalid."""


@dataclass(frozen=True)
class Platform:
    triple: str
    suffix: str
    os_name: str
    cpu: str
    libc: str | None = None

    @property
    def package_name(self) -> str:
        return f"{PLATFORM_PACKAGE_BASE}-{self.suffix}"

    @property
    def binary_name(self) -> str:
        return f"uqa.{self.suffix}.node"


PLATFORMS = (
    Platform("x86_64-unknown-linux-gnu", "linux-x64-gnu", "linux", "x64", "glibc"),
    Platform("aarch64-unknown-linux-gnu", "linux-arm64-gnu", "linux", "arm64", "glibc"),
    Platform("x86_64-apple-darwin", "darwin-x64", "darwin", "x64"),
    Platform("aarch64-apple-darwin", "darwin-arm64", "darwin", "arm64"),
    Platform("x86_64-pc-windows-msvc", "win32-x64-msvc", "win32", "x64"),
    Platform("aarch64-pc-windows-msvc", "win32-arm64-msvc", "win32", "arm64"),
)
PLATFORM_BY_PACKAGE = {platform.package_name: platform for platform in PLATFORMS}
NODE_PACKAGE_NAMES = (*PLATFORM_BY_PACKAGE, ROOT_PACKAGE)
ALL_PACKAGE_NAMES = (*NODE_PACKAGE_NAMES, WASM_PACKAGE)


def load_json(path: pathlib.Path) -> dict[str, object]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReleaseError(f"cannot read JSON file {path}: {error}") from error
    if not isinstance(payload, dict):
        raise ReleaseError(f"JSON file must contain an object: {path}")
    return payload


def write_json(path: pathlib.Path, payload: dict[str, object]) -> None:
    path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def canonical_legal_payloads() -> dict[str, bytes]:
    payloads: dict[str, bytes] = {}
    for relative in LEGAL_FILES:
        source = (
            NODE_ROOT / relative if relative == "LICENSE-NOTICE.md" else ROOT / relative
        )
        try:
            payloads[relative] = source.read_bytes()
        except OSError as error:
            raise ReleaseError(
                f"cannot read canonical legal file {source}: {error}"
            ) from error
    return payloads


def tarball_filename(package_name: str, version: str) -> str:
    unscoped = (
        package_name[1:].replace("/", "-")
        if package_name.startswith("@")
        else package_name
    )
    return f"{unscoped}-{version}.tgz"


def expected_filenames(
    version: str, names: tuple[str, ...] = ALL_PACKAGE_NAMES
) -> dict[str, str]:
    return {name: tarball_filename(name, version) for name in names}


def require_equal(actual: object, expected: object, label: str) -> None:
    if actual != expected:
        raise ReleaseError(f"{label} mismatch: expected {expected!r}, got {actual!r}")


def validate_loader(loader: str, version: str, label: str) -> None:
    if re.search(r"require\(['\"]uqa-[^'\"]+['\"]\)", loader):
        raise ReleaseError(f"{label} still references unscoped platform packages")
    for platform in PLATFORMS:
        if platform.package_name not in loader:
            raise ReleaseError(f"{label} omits {platform.package_name}")
    loader_versions = set(re.findall(r"bindingPackageVersion !== '([^']+)'", loader))
    require_equal(loader_versions, {version}, f"{label} versions")


def validate_node_source(manifest: dict[str, object]) -> str:
    require_equal(manifest.get("name"), ROOT_PACKAGE, "Node.js root package name")
    version = manifest.get("version")
    if not isinstance(version, str) or not version:
        raise ReleaseError("Node.js root package has no valid version")
    require_equal(
        manifest.get("publishConfig"), PUBLISH_CONFIG, "Node.js publishConfig"
    )
    files = manifest.get("files")
    if not isinstance(files, list):
        raise ReleaseError("Node.js root package files must be an array")
    missing_files = sorted(set(ROOT_PACKAGE_FILES) - set(files))
    if missing_files:
        raise ReleaseError(f"Node.js root package omits files: {missing_files}")
    if any(isinstance(item, str) and item.endswith(".node") for item in files):
        raise ReleaseError("Node.js root package must not bundle native addons")
    napi = manifest.get("napi")
    if not isinstance(napi, dict):
        raise ReleaseError("Node.js root package has no napi configuration")
    require_equal(napi.get("binaryName"), "uqa", "napi binaryName")
    require_equal(napi.get("packageName"), PLATFORM_PACKAGE_BASE, "napi packageName")
    require_equal(
        napi.get("targets"), [platform.triple for platform in PLATFORMS], "napi targets"
    )

    try:
        loader = (NODE_ROOT / "index.js").read_text(encoding="utf-8")
    except OSError as error:
        raise ReleaseError(f"cannot read generated Node.js loader: {error}") from error
    validate_loader(loader, version, "generated Node.js loader")
    return version


def copy_file(
    source_root: pathlib.Path, destination_root: pathlib.Path, relative: str
) -> None:
    source = source_root / relative
    destination = destination_root / relative
    try:
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
    except OSError as error:
        raise ReleaseError(f"cannot copy {source} to {destination}: {error}") from error


def platform_manifest(
    root_manifest: dict[str, object], platform: Platform
) -> dict[str, object]:
    manifest: dict[str, object] = {
        "name": platform.package_name,
        "version": root_manifest["version"],
        "description": f"{root_manifest['description']} ({platform.triple})",
        "license": root_manifest["license"],
        "author": root_manifest["author"],
        "repository": root_manifest["repository"],
        "homepage": root_manifest["homepage"],
        "main": platform.binary_name,
        "files": [platform.binary_name, "README.md", *LEGAL_FILES],
        "os": [platform.os_name],
        "cpu": [platform.cpu],
        "engines": root_manifest["engines"],
        "publishConfig": PUBLISH_CONFIG,
    }
    if platform.libc is not None:
        manifest["libc"] = [platform.libc]
    return manifest


def platform_readme(platform: Platform) -> str:
    return (
        f"# `{platform.package_name}`\n\n"
        f"This package contains the prebuilt `{platform.triple}` native addon for `uqa`.\n\n"
        "Install `@cognica-io/uqa` instead of depending on this package directly; npm selects the matching optional package for the current platform.\n"
    )


def pack_stage(
    npm: str,
    stage: pathlib.Path,
    output_dir: pathlib.Path,
    cache_dir: pathlib.Path,
    expected_name: str,
) -> pathlib.Path:
    environment = os.environ.copy()
    environment["npm_config_cache"] = str(cache_dir)
    result = subprocess.run(
        [
            npm,
            "pack",
            "--json",
            "--ignore-scripts",
            "--pack-destination",
            str(output_dir),
        ],
        cwd=stage,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise ReleaseError(
            f"npm pack failed in {stage}:\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    try:
        metadata = json.loads(result.stdout)
        filename = metadata[0]["filename"]
    except (json.JSONDecodeError, IndexError, KeyError, TypeError) as error:
        raise ReleaseError(
            f"npm pack returned invalid metadata in {stage}: {result.stdout}"
        ) from error
    if filename != expected_name:
        raise ReleaseError(f"npm pack created {filename}, expected {expected_name}")
    archive = output_dir / filename
    if not archive.is_file():
        raise ReleaseError(f"npm pack did not create {archive}")
    return archive


def collect_native_artifacts(artifacts_dir: pathlib.Path) -> dict[str, pathlib.Path]:
    expected = {platform.binary_name for platform in PLATFORMS}
    discovered: dict[str, pathlib.Path] = {}
    try:
        candidates = sorted(artifacts_dir.rglob("*.node"))
    except OSError as error:
        raise ReleaseError(
            f"cannot scan native artifacts under {artifacts_dir}: {error}"
        ) from error
    for path in candidates:
        if path.name not in expected:
            raise ReleaseError(f"unexpected native artifact: {path}")
        if path.name in discovered:
            raise ReleaseError(
                f"duplicate native artifact {path.name}: {discovered[path.name]} and {path}"
            )
        if not path.is_file() or path.stat().st_size == 0:
            raise ReleaseError(f"native artifact is empty or not a file: {path}")
        discovered[path.name] = path
    missing = sorted(expected - set(discovered))
    if missing:
        raise ReleaseError(f"missing native artifacts: {missing}")
    return discovered


def build_node_packages(args: argparse.Namespace) -> int:
    artifacts_dir = args.artifacts_dir.resolve()
    output_dir = args.output_dir.resolve()
    if not artifacts_dir.is_dir():
        raise ReleaseError(f"native artifact directory does not exist: {artifacts_dir}")
    output_dir.mkdir(parents=True, exist_ok=True)
    root_manifest = load_json(NODE_ROOT / "package.json")
    version = validate_node_source(root_manifest)
    filenames = expected_filenames(version, NODE_PACKAGE_NAMES)
    collisions = [
        output_dir / filename
        for filename in filenames.values()
        if (output_dir / filename).exists()
    ]
    if collisions:
        raise ReleaseError(f"refusing to overwrite npm archives: {collisions}")
    artifacts = collect_native_artifacts(artifacts_dir)

    with tempfile.TemporaryDirectory(prefix="uqa-node-npm-") as temporary:
        temporary_root = pathlib.Path(temporary)
        cache_dir = temporary_root / "npm-cache"
        root_stage = temporary_root / "root"
        root_stage.mkdir()
        for relative in ROOT_PACKAGE_FILES:
            copy_file(NODE_ROOT, root_stage, relative)
        staged_root_manifest = dict(root_manifest)
        staged_root_manifest["optionalDependencies"] = {
            platform.package_name: version for platform in PLATFORMS
        }
        write_json(root_stage / "package.json", staged_root_manifest)

        archives: list[pathlib.Path] = []
        for platform in PLATFORMS:
            platform_stage = temporary_root / platform.suffix
            platform_stage.mkdir()
            write_json(
                platform_stage / "package.json",
                platform_manifest(root_manifest, platform),
            )
            (platform_stage / "README.md").write_text(
                platform_readme(platform), encoding="utf-8"
            )
            for relative in LEGAL_FILES:
                copy_file(NODE_ROOT, platform_stage, relative)
            shutil.copyfile(
                artifacts[platform.binary_name], platform_stage / platform.binary_name
            )
            archives.append(
                pack_stage(
                    args.npm,
                    platform_stage,
                    output_dir,
                    cache_dir,
                    filenames[platform.package_name],
                )
            )
        archives.append(
            pack_stage(
                args.npm,
                root_stage,
                output_dir,
                cache_dir,
                filenames[ROOT_PACKAGE],
            )
        )

    validate_archives(archives, version, NODE_PACKAGE_NAMES)
    print(f"Built and verified {len(archives)} Node.js npm packages in {output_dir}")
    return 0


def archive_members(path: pathlib.Path) -> dict[str, bytes]:
    try:
        with tarfile.open(path, "r:gz") as archive:
            members: dict[str, bytes] = {}
            for member in archive.getmembers():
                if not member.isfile():
                    continue
                extracted = archive.extractfile(member)
                if extracted is None:
                    continue
                if member.name in members:
                    raise ReleaseError(
                        f"npm archive contains a duplicate member: {path}: {member.name}"
                    )
                members[member.name] = extracted.read()
            return members
    except (OSError, tarfile.TarError) as error:
        raise ReleaseError(f"cannot read npm archive {path}: {error}") from error


def package_members(path: pathlib.Path) -> tuple[dict[str, object], dict[str, bytes]]:
    members = archive_members(path)
    try:
        manifest_payload = members["package/package.json"]
        manifest = json.loads(manifest_payload)
    except KeyError as error:
        raise ReleaseError(f"npm archive omits package/package.json: {path}") from error
    except json.JSONDecodeError as error:
        raise ReleaseError(
            f"npm archive has invalid package.json: {path}: {error}"
        ) from error
    if not isinstance(manifest, dict):
        raise ReleaseError(f"npm archive package.json must contain an object: {path}")
    stripped = {
        name.removeprefix("package/"): payload
        for name, payload in members.items()
        if name.startswith("package/")
    }
    return manifest, stripped


def validate_legal_members(path: pathlib.Path, members: dict[str, bytes]) -> None:
    canonical = canonical_legal_payloads()
    for relative, expected in canonical.items():
        actual = members.get(relative)
        if actual is None:
            raise ReleaseError(f"npm archive omits {relative}: {path}")
        if actual != expected:
            raise ReleaseError(
                f"npm archive contains a noncanonical {relative}: {path}"
            )


def validate_common_manifest(
    path: pathlib.Path, manifest: dict[str, object], version: str
) -> str:
    name = manifest.get("name")
    if not isinstance(name, str) or not name:
        raise ReleaseError(f"npm archive has no package name: {path}")
    require_equal(manifest.get("version"), version, f"{name} version")
    require_equal(manifest.get("license"), "AGPL-3.0-only", f"{name} license")
    require_equal(
        manifest.get("publishConfig"), PUBLISH_CONFIG, f"{name} publishConfig"
    )
    require_equal(
        path.name, tarball_filename(name, version), f"{name} archive filename"
    )
    return name


def validate_root_package(
    path: pathlib.Path,
    manifest: dict[str, object],
    members: dict[str, bytes],
    version: str,
) -> None:
    require_equal(manifest.get("main"), "api.js", "uqa main")
    optional = {platform.package_name: version for platform in PLATFORMS}
    require_equal(
        manifest.get("optionalDependencies"), optional, "uqa optionalDependencies"
    )
    native_members = sorted(name for name in members if name.endswith(".node"))
    if native_members:
        raise ReleaseError(
            f"root uqa package bundles native addons: {path}: {native_members}"
        )
    for relative in ("README.md", "api.js", "index.js", "index.d.ts"):
        if relative not in members:
            raise ReleaseError(f"root uqa package omits {relative}: {path}")
    try:
        loader = members["index.js"].decode("utf-8")
    except UnicodeDecodeError as error:
        raise ReleaseError(
            f"root uqa package has a non-UTF-8 loader: {path}"
        ) from error
    validate_loader(loader, version, "packed Node.js loader")


def validate_platform_package(
    path: pathlib.Path,
    manifest: dict[str, object],
    members: dict[str, bytes],
    platform: Platform,
) -> None:
    require_equal(
        manifest.get("main"), platform.binary_name, f"{platform.package_name} main"
    )
    require_equal(manifest.get("os"), [platform.os_name], f"{platform.package_name} os")
    require_equal(manifest.get("cpu"), [platform.cpu], f"{platform.package_name} cpu")
    expected_libc = [platform.libc] if platform.libc is not None else None
    require_equal(manifest.get("libc"), expected_libc, f"{platform.package_name} libc")
    native_members = sorted(name for name in members if name.endswith(".node"))
    require_equal(
        native_members, [platform.binary_name], f"{platform.package_name} native files"
    )
    if "README.md" not in members:
        raise ReleaseError(f"platform package omits README.md: {path}")


def validate_wasm_package(
    path: pathlib.Path,
    manifest: dict[str, object],
    members: dict[str, bytes],
) -> None:
    require_equal(manifest.get("main"), "index.mjs", "uqa-wasm main")
    for relative in ("README.md", "index.mjs", "index.d.ts", "uqa.js", "uqa.wasm"):
        if relative not in members:
            raise ReleaseError(f"uqa-wasm package omits {relative}: {path}")


def validate_archives(
    archives: list[pathlib.Path],
    version: str,
    expected_names: tuple[str, ...] = ALL_PACKAGE_NAMES,
) -> dict[str, pathlib.Path]:
    expected = set(expected_names)
    by_name: dict[str, pathlib.Path] = {}
    for path in archives:
        if not path.is_file():
            raise ReleaseError(f"npm archive does not exist: {path}")
        manifest, members = package_members(path)
        name = validate_common_manifest(path, manifest, version)
        if name not in expected:
            raise ReleaseError(f"unexpected npm package {name} in {path}")
        if name in by_name:
            raise ReleaseError(
                f"duplicate npm package {name}: {by_name[name]} and {path}"
            )
        validate_legal_members(path, members)
        if name == ROOT_PACKAGE:
            validate_root_package(path, manifest, members, version)
        elif name == WASM_PACKAGE:
            validate_wasm_package(path, manifest, members)
        else:
            validate_platform_package(
                path, manifest, members, PLATFORM_BY_PACKAGE[name]
            )
        by_name[name] = path
    missing = sorted(expected - set(by_name))
    if missing:
        raise ReleaseError(f"missing npm packages: {missing}")
    if len(by_name) != len(expected):
        raise ReleaseError(
            f"expected {len(expected)} npm packages, found {len(by_name)}"
        )
    return by_name


def local_integrity(path: pathlib.Path) -> tuple[str, str]:
    payload = path.read_bytes()
    integrity = "sha512-" + base64.b64encode(hashlib.sha512(payload).digest()).decode(
        "ascii"
    )
    return integrity, hashlib.sha1(payload).hexdigest()  # noqa: S324 - npm registry compatibility


def registry_package(package_name: str, version: str) -> dict[str, object] | None:
    encoded_name = urllib.parse.quote(package_name, safe="")
    request = urllib.request.Request(
        f"{NPM_REGISTRY}{encoded_name}",
        headers={
            "Accept": "application/vnd.npm.install-v1+json",
            "User-Agent": "uqa-engine-npm-release-workflow",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise ReleaseError(
            f"npm registry request failed for {package_name}@{version}: {error}"
        ) from error
    except (OSError, json.JSONDecodeError) as error:
        raise ReleaseError(
            f"cannot read npm registry metadata for {package_name}@{version}: {error}"
        ) from error
    if not isinstance(payload, dict):
        raise ReleaseError(
            f"npm registry returned invalid metadata for {package_name}@{version}"
        )
    versions = payload.get("versions")
    if not isinstance(versions, dict):
        raise ReleaseError(
            f"npm registry packument omits versions for {package_name}@{version}"
        )
    package = versions.get(version)
    if package is None:
        return None
    if not isinstance(package, dict):
        raise ReleaseError(
            f"npm registry returned invalid version metadata for {package_name}@{version}"
        )
    return package


def registry_pending(
    by_name: dict[str, pathlib.Path], version: str
) -> list[pathlib.Path]:
    pending: list[pathlib.Path] = []
    for name in ALL_PACKAGE_NAMES:
        path = by_name[name]
        remote = registry_package(name, version)
        if remote is None:
            pending.append(path)
            continue
        require_equal(remote.get("name"), name, f"npm registry {name} name")
        require_equal(remote.get("version"), version, f"npm registry {name} version")
        distribution = remote.get("dist")
        if not isinstance(distribution, dict):
            raise ReleaseError(f"npm registry metadata omits dist for {name}@{version}")
        integrity, shasum = local_integrity(path)
        require_equal(
            distribution.get("integrity"), integrity, f"npm registry {name} integrity"
        )
        require_equal(distribution.get("shasum"), shasum, f"npm registry {name} shasum")
    return pending


def check_packages(args: argparse.Namespace) -> int:
    archives = [path.resolve() for path in args.archives]
    by_name = validate_archives(archives, args.version)
    pending: list[pathlib.Path] = []
    if args.check_registry or args.require_published:
        pending = registry_pending(by_name, args.version)
    if args.pending_file is not None:
        args.pending_file.write_text(
            "".join(f"{path.name}\n" for path in pending),
            encoding="utf-8",
        )
    if args.require_published and pending:
        raise ReleaseError(
            f"npm registry is missing packages: {[path.name for path in pending]}"
        )
    registry_summary = (
        f"; {len(pending)} pending publication" if args.check_registry else ""
    )
    print(f"npm release contract OK: {len(by_name)} package(s){registry_summary}")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    build = subparsers.add_parser(
        "build-node", help="build the root and platform Node.js packages"
    )
    build.add_argument("--artifacts-dir", type=pathlib.Path, required=True)
    build.add_argument("--output-dir", type=pathlib.Path, required=True)
    build.add_argument("--npm", default="npm")
    build.set_defaults(handler=build_node_packages)

    check = subparsers.add_parser(
        "check", help="verify a complete npm release inventory"
    )
    check.add_argument("--version", required=True)
    check.add_argument("--check-registry", action="store_true")
    check.add_argument("--require-published", action="store_true")
    check.add_argument("--pending-file", type=pathlib.Path)
    check.add_argument("archives", nargs="+", type=pathlib.Path)
    check.set_defaults(handler=check_packages)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    return args.handler(args)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ReleaseError as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
