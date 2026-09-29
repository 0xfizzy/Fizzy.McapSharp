"""Native host builds and complete, provenance-checked NuGet packaging."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET
import traceback

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "win-x64": ("x86_64-pc-windows-msvc", "fizzy_mcap_native.dll"),
    "linux-x64": ("x86_64-unknown-linux-gnu", "libfizzy_mcap_native.so"),
    "linux-arm64": ("aarch64-unknown-linux-gnu", "libfizzy_mcap_native.so"),
}


def run(*args, **kwargs):
    print("+ " + " ".join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), cwd=ROOT, check=True, **kwargs)


def output(*args):
    return subprocess.check_output(list(map(str, args)), cwd=ROOT, text=True).strip()


def host_rid():
    machine = platform.machine().lower()
    arch = {"amd64": "x64", "x86_64": "x64", "aarch64": "arm64", "arm64": "arm64"}.get(machine, machine)
    system = {"Windows": "win", "Linux": "linux"}.get(platform.system(), "unsupported")
    rid = f"{system}-{arch}"
    if rid not in TARGETS or (system == "linux" and platform.libc_ver()[0] != "glibc"):
        raise RuntimeError(f"Unsupported build host: {rid}; Linux requires glibc")
    return rid


def version():
    return ET.parse(ROOT / "Fizzy.McapSharp.csproj").findtext("./PropertyGroup/Version")


def source_identity():
    # Include uncommitted/untracked source bytes so local artifacts cannot silently mix revisions.
    paths = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=ROOT).decode("utf-8").rstrip("\0").split("\0")
    digest = hashlib.sha256()
    for name in sorted(set(paths)):
        path = ROOT / name
        digest.update(name.encode() + b"\0")
        if path.is_file():
            # Git may check out LF or CRLF on different runners.
            data = path.read_bytes()
            digest.update(data.replace(b"\r\n", b"\n") if b"\0" not in data else data)
        else:
            digest.update(b"<missing>")
    return {"commit": output("git", "rev-parse", "HEAD"), "source_sha256": digest.hexdigest()}


def check_binary(path, rid):
    data = path.read_bytes()
    if rid == "win-x64":
        if data[:2] != b"MZ":
            raise RuntimeError("Expected PE library")
        offset = int.from_bytes(data[60:64], "little")
        if data[offset:offset+6] != b"PE\0\0\x64\x86":
            raise RuntimeError("Expected x64 PE library")
    else:
        machine = 62 if rid == "linux-x64" else 183
        if data[:6] != b"\x7fELF\x02\x01" or int.from_bytes(data[18:20], "little") != machine:
            raise RuntimeError(f"Wrong ELF architecture for {rid}")


def audit_linux(path):
    symbols = output("readelf", "--version-info", path)
    versions = [tuple(map(int, v.split("."))) for v in re.findall(r"GLIBC_([0-9.]+)", symbols)]
    if not versions or max(versions) > (2, 35):
        raise RuntimeError(f"GLIBC baseline exceeded or missing: {versions}")
    dynamic = output("readelf", "-d", path)
    needed = re.findall(r"Shared library: \[(.*?)\]", dynamic)
    allowed = {"libgcc_s.so.1", "libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2", "librt.so.1", "ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1"}
    if not needed or set(needed) - allowed or "(RPATH)" in dynamic or "(RUNPATH)" in dynamic:
        raise RuntimeError(f"Unexpected ELF dependencies/search path: {dynamic}")
    print(f"ELF audit: GLIBC <= {max(versions)}, NEEDED={needed}")


def verify_assets():
    identity = source_identity()
    for rid, (target, filename) in TARGETS.items():
        folder = ROOT / "artifacts/native" / rid
        path = folder / filename
        if not path.is_file() or not (folder / "manifest.json").is_file():
            raise RuntimeError(f"Missing {rid} native asset or manifest; collect all three platforms before packing")
        manifest = json.loads((folder / "manifest.json").read_text(encoding="utf-8"))
        expected = dict(identity, rid=rid, target=target, sha256=hashlib.sha256(path.read_bytes()).hexdigest())
        if manifest != expected:
            raise RuntimeError(f"Stale or mismatched native asset: {rid}; rebuild from the same source")
        check_binary(path, rid)
    print("All three native assets match this source and commit.")


def build(test=False):
    rid = host_rid()
    target, filename = TARGETS[rid]
    local_cargo = ROOT / ".tools/cargo/bin/cargo.exe"
    cargo = "cargo"
    if platform.system() == "Windows" and local_cargo.exists():
        cargo = str(local_cargo)
        os.environ["CARGO_HOME"] = str(ROOT / ".tools/cargo")
        os.environ["RUSTUP_HOME"] = str(ROOT / ".tools/rustup")
    if "target-cpu" in os.environ.get("RUSTFLAGS", "") or "target-cpu" in os.environ.get("CARGO_ENCODED_RUSTFLAGS", ""):
        raise RuntimeError("Do not override the portable target CPU baseline")
    # Explicit target directory prevents user Cargo settings from mixing artifacts.
    run(cargo, "build", "--release", "--locked", "--target", target,
        "--target-dir", ROOT / "native/target", "--manifest-path", ROOT / "native/Cargo.toml")
    source = ROOT / "native/target" / target / "release" / filename
    check_binary(source, rid)
    if rid.startswith("linux-"):
        audit_linux(source)
    folder = ROOT / "artifacts/native" / rid
    folder.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, folder / filename)
    manifest = dict(source_identity(), rid=rid, target=target, sha256=hashlib.sha256(source.read_bytes()).hexdigest())
    (folder / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    run("dotnet", "build", ROOT / "Fizzy.McapSharp.csproj", "-c", "Release")
    if test:
        run(cargo, "test", "--release", "--locked", "--target", target,
            "--target-dir", ROOT / "native/target", "--manifest-path", ROOT / "native/Cargo.toml")
        run(sys.executable, "scripts/check_api_coverage.py")
        run(sys.executable, "-m", "unittest", "discover", "-s", "tests", "-p", "test_*.py")
        run("dotnet", "test", ROOT / "tests/Fizzy.McapSharp.Tests", "-c", "Release",
            "--logger", "trx;LogFileName=managed.trx", "--results-directory", ROOT / "artifacts/reports")
        run("dotnet", "run", "--project", ROOT / "tests/Allocations", "-c", "Release")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["build", "pack", "verify-assets"])
    parser.add_argument("--test", action="store_true")
    parser.add_argument("--pack", action="store_true", help="Pack all three previously staged platform assets after building")
    args = parser.parse_args()
    if args.command == "build":
        build(args.test)
    if args.command == "verify-assets":
        verify_assets()
    if args.command == "pack" or args.pack:
        verify_assets()
        run("dotnet", "pack", ROOT / "Fizzy.McapSharp.csproj", "-c", "Release", "-o", ROOT / "artifacts/packages")


if __name__ == "__main__":
    report = {"status": "running", "command": sys.argv[1:]}
    directory = ROOT / "artifacts/reports"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "build.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    try:
        main()
        report["status"] = "passed"
    except BaseException:
        report["status"] = "failed"
        report["error"] = traceback.format_exc()
        raise
    finally:
        directory = ROOT / "artifacts/reports"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "build.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
