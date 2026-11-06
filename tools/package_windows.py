"""Build and package a portable Windows x64 preview from a clean origin/main."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import zipfile


ROOT = Path(__file__).resolve().parent.parent
TARGET = "x86_64-pc-windows-msvc"
BINARIES = {"assembly-scanner": 2, "eipmaster-cli": 3}
ASSETS = (
    "LICENSE", "QUICKSTART.md", "README.md", "ROADMAP.md", "VERIFICATION.md",
    "samples/loopback.eds", "samples/loopback.toml", "samples/mapped-loopback.toml",
    "tools/loopback_adapter.py", "tools/join_capture.py", "tools/test_cli.py", "tools/benchmark.py",
    "tools/test_recovery.py",
)
NOTICE_FILE = re.compile(
    r"(^|[-_.])(licen[cs]e|copying|copyright|notice|ofl|ufl|unlicense)([-_.]|$)", re.I
)
NOTICE_ROOT = "tools/preview_notices"
NOTICE_OVERRIDES = {
    "clipboard-win@5.4.3": "clipboard-win",
    "ethernetip-connections@0.1.1": "ethernetip",
    "ethernetip-core@0.1.1": "ethernetip",
    "gl_generator@0.14.0": "gl-rs",
    "khronos_api@3.1.0": "gl-rs",
    "profiling@1.0.18": "profiling",
    **{f"{name}@0.31.1": "egui" for name in (
        "ecolor", "eframe", "egui", "egui-winit", "egui_glow", "emath",
        "epaint", "epaint_default_fonts",
    )},
}


def command(*args, env=None):
    return subprocess.check_output(args, cwd=ROOT, env=env)


def verified_commit():
    if command("git", "status", "--porcelain", "--untracked-files=normal").strip():
        raise ValueError("Start from a clean checkout. Use an ignored or external output directory.")
    commit = command("git", "rev-parse", "HEAD").decode().strip()
    upstream = command("git", "rev-parse", "refs/remotes/origin/main").decode().strip()
    if commit != upstream:
        raise ValueError("HEAD must match origin/main. Fetch and integrate origin/main before packaging.")
    return commit


def tracked(commit, path):
    return command("git", "show", f"{commit}:{path}")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def executable_imports(data, subsystem):
    """Check PE32+ x64 and return the names in the direct import table."""
    if data[:2] != b"MZ":
        raise ValueError("Expected a Windows executable")
    pe, = struct.unpack_from("<I", data, 0x3C)
    machine, section_count = struct.unpack_from("<HH", data, pe + 4)
    optional = pe + 24
    optional_size, = struct.unpack_from("<H", data, pe + 20)
    if (data[pe:pe + 4] != b"PE\0\0" or machine != 0x8664
            or struct.unpack_from("<H", data, optional)[0] != 0x20B
            or struct.unpack_from("<H", data, optional + 68)[0] != subsystem):
        raise ValueError("Unexpected architecture or subsystem in packaged executable")

    sections = [struct.unpack_from("<III", data, optional + optional_size + 40*i + 12)
                for i in range(section_count)]

    def offset(rva):
        for address, length, raw in sections:
            if address <= rva < address + length:
                return raw + rva - address
        raise ValueError("Import address lies outside the PE file sections")

    imports, size = struct.unpack_from("<II", data, optional + 120)
    start = offset(imports)
    names = set()
    for pos in range(start, start + size, 20):
        descriptor = struct.unpack_from("<IIIII", data, pos)
        if descriptor == (0, 0, 0, 0, 0):
            break
        name = offset(descriptor[3])
        dll = data[name:data.index(0, name, name + 256)].decode("ascii").lower()
        if dll.startswith(("vcruntime", "msvcp", "msvcr", "ucrtbase", "api-ms-win-crt-")):
            raise ValueError(f"Expected the static C runtime but the executable imports {dll}")
        names.add(dll)
    return sorted(names)


def dependency_notices(metadata, commit, sysroot):
    result = {}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    pending, reachable = [root], set()
    while pending:
        package_id = pending.pop()
        if package_id in reachable:
            continue
        reachable.add(package_id)
        pending.extend(dep["pkg"] for dep in nodes[package_id]["deps"]
                       if any(kind["kind"] != "dev" for kind in dep["dep_kinds"]))
    reachable.remove(root)
    packages = sorted((p for p in metadata["packages"] if p["id"] in reachable),
                      key=lambda p: (p["name"], p["version"]))
    index = ["# Third-party notices", "",
             "These packages are reachable in Cargo's Windows dependency graph. "
             "The list also includes build dependencies.", "",
             "| Cargo package | Declared license | Packaged notices |", "|---|---|---|"]
    groups = set()
    for package in packages:
        name = f"{package['name']}@{package['version']}"
        source = Path(package["manifest_path"]).parent
        directory = f"{package['name']}-{package['version']}"
        files = {path for path in source.rglob("*") if path.is_file() and NOTICE_FILE.search(path.name)}
        if package["license_file"]:
            files.add(source / package["license_file"])
        override = NOTICE_OVERRIDES.get(name)
        if not files and not override:
            raise ValueError(f"Review the upstream license files for {name} before packaging")
        locations = []
        if files:
            for file in files:
                result[f"third-party/{directory}/{file.relative_to(source).as_posix()}"] = file.read_bytes()
            locations.append(f"[Crate files]({directory}/)")
        if override:
            groups.add(override)
            locations.append(f"[Upstream files](upstream/{override}/)")
        declared = (package["license"] or "See license file").replace("|", "\\|")
        index.append(f"| {name} | {declared} | {' and '.join(locations)} |")

    for group in sorted(groups):
        prefix = f"{NOTICE_ROOT}/{group}/"
        paths = command("git", "ls-tree", "-r", "--name-only", commit, "--", prefix).decode().splitlines()
        if not paths:
            raise ValueError(f"Missing pinned upstream notices for {group}")
        for path in paths:
            result[f"third-party/upstream/{path.removeprefix(NOTICE_ROOT + '/')}"] = tracked(commit, path)
    result["third-party/upstream/INDEX.md"] = tracked(commit, f"{NOTICE_ROOT}/INDEX.md")

    rust_docs = sysroot / "share" / "doc" / "rust"
    result["third-party/rust/COPYRIGHT-library.html"] = (rust_docs / "COPYRIGHT-library.html").read_bytes()
    for license_path in (rust_docs / "licenses").glob("*.txt"):
        result[f"third-party/rust/licenses/{license_path.name}"] = license_path.read_bytes()
    index.extend(["", "[Rust standard library notices](rust/COPYRIGHT-library.html)",
                  "", "[Pinned upstream notice sources](upstream/INDEX.md)"])
    result["third-party/INDEX.md"] = ("\n".join(index) + "\n").encode()
    return result


def package(args):
    if os.name != "nt":
        raise ValueError("Build the preview on Windows with the x64 MSVC Rust toolchain")
    commit = verified_commit()
    output = args.output.resolve()
    if output.is_relative_to(ROOT):
        probe = (output / "preview.zip").relative_to(ROOT)
        command("git", "check-ignore", "--", probe.as_posix())
    cargo = ("cargo", "+" + args.toolchain)
    rustc = ("rustc", "+" + args.toolchain)
    offline = ("--offline",) if args.offline else ()
    metadata = json.loads(command(*cargo, "metadata", "--format-version", "1", "--locked",
                                  "--filter-platform", TARGET, *offline))
    root_id = metadata["resolve"]["root"]
    version = next(p["version"] for p in metadata["packages"] if p["id"] == root_id)
    name = f"EIPMaster-{version}-{commit[:12]}-win-x64"
    archive = output / (name + ".zip")
    checksum = archive.with_suffix(".zip.sha256")
    if archive.exists() or checksum.exists():
        raise ValueError(f"Choose an output directory without an existing {name} bundle")

    flags = ["-C", "target-feature=+crt-static", "-C", "link-arg=/Brepro",
             "-C", "link-arg=/INCREMENTAL:NO"]
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve()
    target_dir = args.target_dir.resolve()
    for path, replacement in ((ROOT, "eipmaster"), (cargo_home, "cargo-home"),
                              (target_dir, "target-build")):
        for spelling in dict.fromkeys((str(path), path.as_posix())):
            flags.extend(("--remap-path-prefix", f"{spelling}={replacement}"))
    env = os.environ | {"CARGO_ENCODED_RUSTFLAGS": "\x1f".join(flags), "CARGO_INCREMENTAL": "0"}
    built = subprocess.run([
        *cargo, "build", "--locked", "--release", "--target", TARGET, "--bins",
        "--target-dir", str(target_dir), "--message-format=json-render-diagnostics",
        *offline,
    ], cwd=ROOT, env=env, stdout=subprocess.PIPE)
    executables = {}
    for line in built.stdout.splitlines():
        event = json.loads(line)
        rendered = event.get("message", {}).get("rendered")
        if rendered:
            print(rendered, file=sys.stderr, end="")
        if event["reason"] == "compiler-artifact" and event["package_id"] == root_id:
            if event["target"]["name"] in BINARIES and event["executable"]:
                executables[event["target"]["name"]] = Path(event["executable"])
    built.check_returncode()
    if executables.keys() != BINARIES.keys() or verified_commit() != commit:
        raise ValueError("Build did not produce both executables from the verified commit")

    payload = {path: tracked(commit, path) for path in ASSETS}
    imports = {}
    for binary, subsystem in BINARIES.items():
        filename = binary + ".exe"
        data = executables[binary].read_bytes()
        imports[filename] = executable_imports(data, subsystem)
        payload[filename] = data
    sysroot = Path(command(*rustc, "--print", "sysroot").decode().strip())
    payload.update(dependency_notices(metadata, commit, sysroot))
    info = {
        "version": version, "git_commit": commit, "target": TARGET, "profile": "release",
        "rustc": command(*rustc, "-vV").decode().splitlines(),
        "cargo": command(*cargo, "--version").decode().strip(),
        "cargo_lock_sha256": digest(tracked(commit, "Cargo.lock")),
        "codegen_flags": flags[:6],
        "source_paths_remapped": True,
        "direct_executable_imports": imports,
        "hardware_qualification": "See VERIFICATION.md and the physical bench acceptance issue.",
    }
    payload["BUILD-INFO.json"] = (json.dumps(info, indent=2, sort_keys=True) + "\n").encode()
    payload["SHA256SUMS.txt"] = "".join(
        f"{digest(data)}  {path}\n" for path, data in sorted(payload.items())
    ).encode()

    output.mkdir(parents=True, exist_ok=True)
    stream = archive.open("xb")
    checksum_created = False
    try:
        with stream, zipfile.ZipFile(stream, "w") as bundle:
            for path, data in sorted(payload.items()):
                member = zipfile.ZipInfo(f"{name}/{path}", date_time=(1980, 1, 1, 0, 0, 0))
                member.create_system = 3
                member.external_attr = 0o100644 << 16
                bundle.writestr(member, data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=6)
        with zipfile.ZipFile(archive) as bundle:
            if bundle.testzip() is not None:
                raise ValueError("ZIP integrity check failed")
        sha256 = digest(archive.read_bytes())
        with checksum.open("x", encoding="ascii", newline="\n") as stream:
            checksum_created = True
            stream.write(f"{sha256}  {archive.name}\n")
    except Exception:
        archive.unlink()
        if checksum_created:
            checksum.unlink()
        raise
    print(f"{sha256}  {archive}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="External or Git-ignored output directory")
    parser.add_argument("--toolchain", default="stable-x86_64-pc-windows-msvc")
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target" / "preview-package")
    parser.add_argument("--offline", action="store_true", help="Build using the local Cargo cache")
    try:
        package(parser.parse_args())
    except (OSError, ValueError, struct.error, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
