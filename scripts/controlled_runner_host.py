#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Operator-installed Linux host adapter for isolated performance execution."""

from __future__ import annotations

from contextlib import nullcontext
import hashlib
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess

from controlled_performance import analytical_observations, claim_observations
from performance_noise import ANALYTICAL_FILTER
from controlled_performance_resources import (
    MeasurementResources, restrict_workqueues, verify_workqueues, workqueue_mask,
)
from performance_qualification import QualificationError, digest


WORK = Path("/var/lib/uqa-performance/work")
REPOSITORY = WORK / "repository"
CONTROL = Path("/var/lib/uqa-performance/controller")
BINARY_ROOT = Path("/opt/uqa-performance/binaries")
BENCH_CPUS = "8-15"
MEASUREMENT_SLICE = "uqa_performance.slice"


def command(*args: str, cwd: Path | None = None) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class ControlledHost:
    def __init__(self, run_id: str, config: dict):
        self.run_id, self.config = run_id, config
        self.output = CONTROL / "runs" / run_id
        self.output.mkdir(parents=True, exist_ok=False)
        self.workspace = WORK / "measurement" / run_id
        self.account = pwd.getpwnam("uqa-bench")
        self.rust_bin = Path(self.account.pw_dir) / ".cargo" / "bin"
        self.counter = 0
        self.boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        self.toolchain = {name: command("runuser", "-u", "uqa-bench", "--", path, flag)
                          for name, path, flag in (
                              ("rustc", str(self.rust_bin / "rustc"), "-vV"),
                              ("cargo", str(self.rust_bin / "cargo"), "--version"),
                              ("cc", "/usr/bin/gcc", "--version"),
                              ("ld", "/usr/bin/ld", "--version"))}
        self.build_environment = digest({"toolchain": self.toolchain, "profile": "bench",
                                         "incremental": False, "features": "default", "schema": 1})
        self.make_writable(self.workspace)

    def make_writable(self, path: Path):
        path.mkdir(parents=True, exist_ok=True)
        os.chown(path, self.account.pw_uid, self.account.pw_gid)

    def progress(self, stage: str, **details):
        record = {"stage": stage, **details}
        (self.output / "progress.json").write_text(json.dumps(record) + "\n")
        print(json.dumps(record), flush=True)

    def git(self, *args: str, cwd: Path = REPOSITORY) -> str:
        return command("runuser", "-u", "uqa-bench", "--", "git", *args, cwd=cwd)

    def source(self, revision: str, role: str) -> Path:
        # Claims read only their private temporary files. Reuse their checkout for the
        # candidate; the analytical release keeps its own runtime manifest checkout.
        path = REPOSITORY if role == "reference" else WORK / "claims-reference-source"
        if path.exists():
            if self.git("status", "--porcelain", cwd=path):
                raise QualificationError(f"unexpected modified source tree: {role}")
            self.git("checkout", "--detach", revision, cwd=path)
        else:
            self.git("worktree", "add", "--detach", str(path), revision)
        if self.git("rev-parse", "HEAD", cwd=path) != revision:
            raise QualificationError("source revision differs from the requested commit")
        return path

    def unit(self, label: str, argv: list[str], cwd: Path, *, measurement: bool = False,
             writable: Path | None = None, environment: dict | None = None,
             measurement_cpus: str = BENCH_CPUS) -> Path:
        self.counter += 1
        stdout = self.output / (label + ".stdout")
        stderr = self.output / (label + ".stderr")
        args = ["systemd-run", "--quiet", "--wait", "--pipe", "--collect",
                f"--unit=uqa-perf-{self.run_id}-{self.counter}", "--uid=uqa-bench",
                f"--working-directory={cwd}", "--property=NoNewPrivileges=yes",
                "--property=KillMode=control-group", "--property=MemorySwapMax=0",
                "--property=IPAddressDeny=169.254.169.254/32",
                f"--setenv=PATH={self.rust_bin}:/usr/local/bin:/usr/bin:/bin"]
        if measurement:
            if writable is None:
                raise QualificationError("measurement output directory is required")
            args += [f"--slice={MEASUREMENT_SLICE}", f"--property=AllowedCPUs={measurement_cpus}",
                     f"--property=CPUAffinity={measurement_cpus}", "--property=PrivateNetwork=yes",
                     "--property=PrivateTmp=yes", "--property=ProtectSystem=strict",
                     "--property=ProtectHome=yes", f"--property=ReadWritePaths={writable}",
                     "--property=RuntimeMaxSec=300", "--property=TasksMax=256"]
        else:
            args += ["--property=RuntimeMaxSec=5400"]
        args += [f"--setenv={name}={value}" for name, value in (environment or {}).items()]
        group = Path("/sys/fs/cgroup") / MEASUREMENT_SLICE / f"uqa-perf-{self.run_id}-{self.counter}.service"
        resources = (MeasurementResources(group, self.output / (label + ".resources.json"), measurement_cpus,
                                         executable=Path(argv[0]), benchmark_log=stderr)
                     if measurement else nullcontext())
        workload = ["/usr/bin/setarch", "--addr-no-randomize", *argv] if measurement else argv
        with resources, stdout.open("wb") as out, stderr.open("wb") as err:
            result = subprocess.run([*args, *workload], stdout=out, stderr=err, check=False)
            if result.returncode:
                raise QualificationError(f"{label} failed ({result.returncode}); see retained stderr")
        return stdout

    def build(self, revision: str, role: str, claims: bool) -> dict:
        self.progress("build", role=role, revision=revision)
        expected = {"analytical_comparison", "row_claim_contention"} if claims else {"analytical_comparison"}
        for record in BINARY_ROOT.glob("*/artifacts.json"):
            cached = json.loads(record.read_text())
            if set(cached) == expected and all(item["revision"] == revision
                    and item.get("build_environment") == self.build_environment
                    and file_hash(Path(item["path"])) == item["sha256"] for item in cached.values()):
                (self.output / (role + "-build.json")).write_text(json.dumps(cached, indent=2) + "\n")
                return cached
        source = self.source(revision, role)
        args = [str(self.rust_bin / "cargo"), "build", "--profile", "bench", "--locked", "-p", "uqa-engine",
                "--bench", "analytical_comparison", "--message-format=json-render-diagnostics"]
        if claims:
            args += ["-p", "uqa-execution", "--example", "row_claim_contention"]
        output = self.unit("build-" + role, args, source, environment={
            "CARGO_INCREMENTAL": "0", "CARGO_TARGET_DIR": str(REPOSITORY / "target" / "reference"),
            "CC": "ccache gcc", "CXX": "ccache g++"})
        artifacts = {}
        for line in output.read_text().splitlines():
            row = json.loads(line)
            name = row.get("target", {}).get("name")
            if row.get("reason") != "compiler-artifact" or not row.get("executable"):
                continue
            if name not in {"analytical_comparison", "row_claim_contention"}:
                continue
            profile = row["profile"]
            if profile["opt_level"] != "3" or profile["debug_assertions"]:
                raise QualificationError("timing requires the optimized benchmark profile")
            binary = Path(row["executable"]).resolve()
            if not binary.is_relative_to((REPOSITORY / "target" / "reference").resolve()):
                raise QualificationError("Cargo executable escapes its isolated target directory")
            destination = BINARY_ROOT / role / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(binary, destination)
            destination.chmod(0o555)
            artifacts[name] = {"path": str(destination), "sha256": file_hash(destination),
                               "revision": revision, "profile": profile, "features": row["features"],
                               "build_environment": self.build_environment}
        if set(artifacts) != expected:
            raise QualificationError("Cargo omitted a required controlled workload")
        (BINARY_ROOT / role / "artifacts.json").write_text(json.dumps(artifacts, indent=2) + "\n")
        (self.output / (role + "-build.json")).write_text(json.dumps(artifacts, indent=2) + "\n")
        return artifacts

    def isolate(self) -> dict:
        self.progress("establish-host-control")
        if subprocess.run(["pgrep", "-u", str(self.account.pw_uid)], stdout=subprocess.DEVNULL).returncode != 1:
            raise QualificationError("workload account still has running processes before calibration")
        timers = command("systemctl", "list-units", "--type=timer", "--state=active", "--no-legend", "--plain")
        for line in timers.splitlines():
            command("systemctl", "stop", line.split()[0])
        for service in ("apt-daily.service", "apt-daily-upgrade.service", "unattended-upgrades.service",
                        "snapd.service", "snapd.socket", "cron.service"):
            subprocess.run(["systemctl", "stop", service], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        for group in ("system.slice", "user.slice", "init.scope"):
            command("systemctl", "set-property", "--runtime", group, "AllowedCPUs=0-7")
        command("systemctl", "start", MEASUREMENT_SLICE)
        command("systemctl", "set-property", "--runtime", MEASUREMENT_SLICE, f"AllowedCPUs={BENCH_CPUS}")
        restrict_workqueues()
        group = Path("/sys/fs/cgroup") / MEASUREMENT_SLICE
        (group / "cpuset.cpus.exclusive").write_text(BENCH_CPUS)
        (group / "cpuset.cpus.partition").write_text("root")
        if (group / "cpuset.cpus.partition").read_text().strip() != "root":
            raise QualificationError("exclusive measurement CPU partition is invalid")
        if (group / "cpuset.cpus.effective").read_text().strip() != BENCH_CPUS:
            raise QualificationError("measurement CPU set differs")
        if len(Path("/proc/swaps").read_text().splitlines()) != 1:
            raise QualificationError("controlled host must not use swap")
        response = json.loads(command("aws", "ec2", "describe-instances", "--region", self.config["region"],
                                      "--instance-ids", self.config["instance_id"], "--output", "json"))
        instance = response["Reservations"][0]["Instances"][0]
        if (instance["Placement"]["Tenancy"] != "dedicated"
                or instance["InstanceType"] != "c7g.4xlarge"
                or instance["State"]["Name"] != "running"):
            raise QualificationError("EC2 does not match the controlled-host contract")
        evidence = {"instance_id": instance["InstanceId"], "placement": instance["Placement"],
                    "instance_type": instance["InstanceType"], "boot_id": self.boot,
                    "kernel": command("uname", "-a"), "cpu": command("lscpu", "--json"), "toolchain": self.toolchain,
                    "measurement_cpus": BENCH_CPUS, "administration_cpus": "0-7",
                    "partition": "root", "unbound_workqueue_mask": f"{workqueue_mask():x}",
                    "workload_cpu_affinity": {"analytical": "8", "claims": BENCH_CPUS}, "processes": command("ps", "-eo", "pid,uid,comm,cgroup"),
                    "workload_address_randomization": "disabled per process with setarch; verified through procfs",
                    "controller_sha256": {path.name: file_hash(path) for path in Path(__file__).parent.glob("*.py")},
                    "stopped_timers": timers, "swap": Path("/proc/swaps").read_text()}
        (self.output / "host-control.json").write_text(json.dumps(evidence, indent=2) + "\n")
        self.initial_steal = self.steal()
        return evidence

    def steal(self) -> dict:
        return {fields[0]: int(fields[8]) for line in Path("/proc/stat").read_text().splitlines()
                if (fields := line.split())[0] in {f"cpu{cpu}" for cpu in range(8, 16)}}

    def verify_control(self):
        verify_workqueues()
        if Path("/proc/sys/kernel/random/boot_id").read_text().strip() != self.boot:
            raise QualificationError("controlled boot changed during measurement")
        if self.steal() != self.initial_steal:
            raise QualificationError("CPU steal was observed; measurements cannot establish acceptance")
        group = Path("/sys/fs/cgroup") / MEASUREMENT_SLICE
        if (group / "cpuset.cpus.partition").read_text().strip() != "root":
            raise QualificationError("measurement partition changed")
        if (group / "cpuset.cpus.effective").read_text().strip() != BENCH_CPUS:
            raise QualificationError("measurement CPU allocation changed")
        if (group / "cgroup.procs").read_text().strip() or list(group.glob("*.service")):
            raise QualificationError("unexpected process or unit in the measurement partition")

    def measure(self, kind: str, artifact: dict, source: Path | None, label: str, manifest: dict) -> dict:
        if kind == "analytical" and source is None:
            raise QualificationError("analytical workload requires its exact runtime source")
        self.verify_control()
        if file_hash(Path(artifact["path"])) != artifact["sha256"]:
            raise QualificationError("workload executable changed after calibration selection")
        writable = self.workspace / label
        self.make_writable(writable)
        args = [artifact["path"]]
        environment = {}
        if kind == "analytical":
            args += ["--bench", "--noplot", ANALYTICAL_FILTER]
            environment["CRITERION_HOME"] = str(writable / "criterion")
        else:
            args += ["--measure"]
        working_directory = source if kind == "analytical" else writable
        stdout = self.unit(label, args, working_directory, measurement=True, writable=writable,
                           environment=environment, measurement_cpus="8" if kind == "analytical" else BENCH_CPUS)
        self.verify_control()
        if kind == "claims":
            if stdout.stat().st_size > 4 * 1024 * 1024:
                raise QualificationError("oversized claim workload output")
            return claim_observations(json.loads(stdout.read_text()), manifest)
        names = [gate["benchmark"] for gate in manifest["regression_gates"]]
        raw = self.output / "criterion" / label
        for name in names:
            for filename in ("estimates.json", "sample.json"):
                path = writable.joinpath("criterion", *name.split("/"), "new", filename).resolve()
                if not path.is_relative_to(writable.resolve()) or path.stat().st_size > 1024 * 1024:
                    raise QualificationError("invalid controlled Criterion artifact path or size")
                destination = raw.joinpath(*name.split("/"), "new", filename)
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(path.read_bytes())
        return analytical_observations(raw, names, manifest["criterion"]["sample_size"])

    def upload(self):
        command("aws", "s3", "cp", str(self.output),
                f"s3://{self.config['artifact_bucket']}/runs/{self.run_id}/", "--recursive",
                "--only-show-errors", "--region", self.config["region"])

    def sign(self, path: Path):
        command("openssl", "dgst", "-sha256", "-sign", str(CONTROL / "issuer.pem"),
                "-out", str(path.with_suffix(path.suffix + ".sig")), str(path))
