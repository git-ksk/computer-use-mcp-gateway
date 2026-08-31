#!/usr/bin/env python3
"""Build and verify bounded CUMG V2 release-candidate archives."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform as host_platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile

MANIFEST_NAME = "release-artifact-manifest.json"
MANIFEST_SCHEMA_VERSION = 2
MAX_MANIFEST_BYTES = 64 * 1024
MAX_FILE_BYTES = 256 * 1024 * 1024
MAX_ARCHIVE_MEMBER_BYTES = 256 * 1024 * 1024
MAX_ARCHIVE_BYTES = 1024 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 64

COMMON_BINARIES = (
    "v2_hub",
    "v2_agent",
    "v2_maint",
    "v2_keyctl",
    "v2_tls_check",
)
UNIX_BINARIES = ("v2_grant_signer", "v2_handoff_ctl")
PLATFORM_BINARIES = {
    "linux": COMMON_BINARIES + UNIX_BINARIES,
    "macos": COMMON_BINARIES + UNIX_BINARIES + ("v2_doctor", "v2_status", "v2_recover", "v2_recovery_enclave_helper"),
    "windows": COMMON_BINARIES,
}

MACOS_INSTALL_ASSETS = (
    "install/v2_artifact_install.py",
    "install/v2_artifact_payload.py",
    "install/v2_release_candidate.py",
    "install/v2-single-mac-upgrade.sh",
    "install/v2_launchd_maintenance_job.py",
    "install/v2_upgrade_transaction.py",
    "install/v2_launchd_topology_guard.py",
    "install/v2_mutation_authority_preflight.py",
    "install/v2_handoff_runtime_preflight.py",
    "install/v2_handoff_runtime_cleanup.py",
    "install/README.md",
    "install/single-mac-profile.example.json",
    "launchd/com.github.git-ksk.cumg-v2-agent.plist",
    "launchd/com.github.git-ksk.cumg-v2-grant-signer.plist",
    "launchd/com.github.git-ksk.cumg-v2-hub.plist",
    "components/handoff-runtime.tar.gz",
)
INSTALL_PROFILE = "single-mac-artifact-v1"

SEMVER_RE = re.compile(
    r"^[0-9]+\.[0-9]+\.[0-9]+"
    r"(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?"
    r"(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$"
)
COMMIT_RE = re.compile(r"^[0-9a-fA-F]{40}$")
ARCH_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


class CandidateError(RuntimeError):
    """A release candidate is malformed or cannot be verified safely."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def normalized_platform(value: str) -> str:
    normalized = value.strip().lower()
    aliases = {"darwin": "macos", "mac": "macos", "win32": "windows", "win": "windows"}
    normalized = aliases.get(normalized, normalized)
    if normalized not in PLATFORM_BINARIES:
        raise CandidateError(f"unsupported platform: {value!r}")
    return normalized


def normalized_architecture(value: str) -> str:
    normalized = value.strip().lower()
    if not ARCH_RE.fullmatch(normalized):
        raise CandidateError("architecture must be a bounded safe identifier")
    return normalized


def validate_package_version(value: str) -> str:
    value = value.strip()
    if not SEMVER_RE.fullmatch(value):
        raise CandidateError("package version must be SemVer")
    return value


def validate_source_commit(value: str) -> str:
    value = value.strip().lower()
    if not COMMIT_RE.fullmatch(value):
        raise CandidateError("source commit must be exactly 40 hexadecimal characters")
    return value


def executable_suffix(platform_name: str) -> str:
    return ".exe" if platform_name == "windows" else ""


def expected_binary_paths(platform_name: str) -> tuple[str, ...]:
    suffix = executable_suffix(platform_name)
    return tuple(f"bin/{name}{suffix}" for name in PLATFORM_BINARIES[platform_name])


def expected_artifact_paths(platform_name: str) -> tuple[str, ...]:
    binaries = expected_binary_paths(platform_name)
    if platform_name == "macos":
        return binaries + MACOS_INSTALL_ASSETS
    return binaries


def bundle_name(package_version: str, platform_name: str, architecture: str) -> str:
    return f"cumg-v{package_version}-{platform_name}-{architecture}"


def validate_relative_path(value: str) -> PurePosixPath:
    if not value or "\\" in value or ":" in value:
        raise CandidateError(f"unsafe artifact path: {value!r}")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise CandidateError(f"unsafe artifact path: {value!r}")
    if path.as_posix() != value:
        raise CandidateError(f"non-canonical artifact path: {value!r}")
    return path


def require_regular_source(path: Path, platform_name: str) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise CandidateError(f"required binary is missing: {path.name}") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise CandidateError(f"required binary is not a regular file: {path.name}")
    if info.st_size <= 0 or info.st_size > MAX_FILE_BYTES:
        raise CandidateError(f"required binary has invalid size: {path.name}")
    if platform_name != "windows" and info.st_mode & 0o111 == 0:
        raise CandidateError(f"required binary is not executable: {path.name}")


def safe_output_dir(path: Path) -> None:
    if path.exists() and path.is_symlink():
        raise CandidateError("output directory must not be a symlink")
    path.mkdir(parents=True, exist_ok=True)
    if not path.is_dir():
        raise CandidateError("output path is not a directory")


def write_manifest(
    bundle_root: Path,
    package_version: str,
    source_commit: str,
    platform_name: str,
    architecture: str,
    hub_agent_schema_version: int,
    paired_handoff_commit: str | None,
    records: list[dict[str, object]],
) -> None:
    manifest = {
        "schema_version": MANIFEST_SCHEMA_VERSION,
        "package_version": package_version,
        "source_commit": source_commit,
        "platform": platform_name,
        "architecture": architecture,
        "hub_agent_schema_version": hub_agent_schema_version,
        "paired_handoff_commit": paired_handoff_commit,
        "install_profile": INSTALL_PROFILE if platform_name == "macos" else None,
        "files": records,
    }
    encoded = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(encoded) > MAX_MANIFEST_BYTES:
        raise CandidateError("artifact manifest exceeds bounded size")
    (bundle_root / MANIFEST_NAME).write_bytes(encoded)


def canonical_tar_archive(bundle_root: Path, archive: Path) -> None:
    with archive.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as tar:
                paths = [bundle_root] + sorted(bundle_root.rglob("*"), key=lambda p: p.as_posix())
                for path in paths:
                    relative = path.relative_to(bundle_root.parent).as_posix()
                    info = tarfile.TarInfo(relative)
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mtime = 0
                    if path.is_dir():
                        info.type = tarfile.DIRTYPE
                        info.mode = 0o755
                        tar.addfile(info)
                        continue
                    data = path.read_bytes()
                    info.size = len(data)
                    info.mode = 0o755 if path.parent.name == "bin" else 0o644
                    tar.addfile(info, fileobj=_BytesReader(data))


def canonical_zip_archive(bundle_root: Path, archive: Path) -> None:
    with zipfile.ZipFile(archive, mode="x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
        paths = [bundle_root] + sorted(bundle_root.rglob("*"), key=lambda p: p.as_posix())
        for path in paths:
            relative = path.relative_to(bundle_root.parent).as_posix()
            if path.is_dir():
                relative += "/"
                mode = stat.S_IFDIR | 0o755
                payload = b""
            else:
                mode = stat.S_IFREG | (0o755 if path.parent.name == "bin" else 0o644)
                payload = path.read_bytes()
            info = zipfile.ZipInfo(relative, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = mode << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(info, payload, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)


class _BytesReader:
    def __init__(self, data: bytes):
        self._data = data
        self._offset = 0

    def read(self, size: int = -1) -> bytes:
        if size < 0:
            size = len(self._data) - self._offset
        chunk = self._data[self._offset : self._offset + size]
        self._offset += len(chunk)
        return chunk


def build_candidate(args: argparse.Namespace) -> Path:
    binary_dir = Path(args.binary_dir)
    output_dir = Path(args.output_dir)
    platform_name = normalized_platform(args.platform)
    architecture = normalized_architecture(args.architecture)
    package_version = validate_package_version(args.package_version)
    source_commit = validate_source_commit(args.source_commit)
    hub_agent_schema_version = int(args.hub_agent_schema_version)
    if not 1 <= hub_agent_schema_version <= 65535:
        raise CandidateError("Hub/Agent schema version is invalid")
    paired_handoff_commit = None
    payload_dir = None
    if platform_name == "macos":
        paired_handoff_commit = validate_source_commit(args.paired_handoff_commit)
        payload_dir = Path(args.payload_dir)
        if not payload_dir.is_dir() or payload_dir.is_symlink():
            raise CandidateError("macOS install payload directory must be a real directory")

    if not binary_dir.is_dir() or binary_dir.is_symlink():
        raise CandidateError("binary directory must be a real directory")
    safe_output_dir(output_dir)

    name = bundle_name(package_version, platform_name, architecture)
    extension = ".zip" if platform_name == "windows" else ".tar.gz"
    archive = output_dir / f"{name}{extension}"
    checksum = Path(f"{archive}.sha256")
    if archive.exists() or checksum.exists():
        raise CandidateError("refusing to overwrite an existing release-candidate artifact")

    with tempfile.TemporaryDirectory(prefix=".cumg-release-", dir=output_dir) as temporary:
        bundle_root = Path(temporary) / name
        bundle_bin = bundle_root / "bin"
        bundle_bin.mkdir(parents=True, mode=0o755)
        records: list[dict[str, object]] = []

        for relative in expected_binary_paths(platform_name):
            relative_path = validate_relative_path(relative)
            source = binary_dir / relative_path.name
            require_regular_source(source, platform_name)
            destination = bundle_root.joinpath(*relative_path.parts)
            shutil.copyfile(source, destination)
            if platform_name != "windows":
                destination.chmod(0o755)
            records.append(
                {
                    "path": relative,
                    "size": destination.stat().st_size,
                    "sha256": sha256_file(destination),
                }
            )

        if platform_name == "macos":
            assert payload_dir is not None
            for relative in MACOS_INSTALL_ASSETS:
                relative_path = validate_relative_path(relative)
                source = payload_dir.joinpath(*relative_path.parts)
                try:
                    info = source.lstat()
                except FileNotFoundError as exc:
                    raise CandidateError(f"required install asset is missing: {relative}") from exc
                if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
                    raise CandidateError(f"required install asset is not regular: {relative}")
                if info.st_size <= 0 or info.st_size > MAX_FILE_BYTES:
                    raise CandidateError(f"required install asset has invalid size: {relative}")
                destination = bundle_root.joinpath(*relative_path.parts)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, destination)
                if relative.startswith("install/"):
                    destination.chmod(0o755)
                records.append({"path": relative, "size": destination.stat().st_size, "sha256": sha256_file(destination)})

        write_manifest(
            bundle_root,
            package_version,
            source_commit,
            platform_name,
            architecture,
            hub_agent_schema_version,
            paired_handoff_commit,
            records,
        )
        verify_bundle_dir(bundle_root)
        if platform_name == "windows":
            canonical_zip_archive(bundle_root, archive)
        else:
            canonical_tar_archive(bundle_root, archive)

    digest = sha256_file(archive)
    checksum.write_text(f"{digest}  {archive.name}\n", encoding="ascii")
    print(f"BUILT archive={archive} checksum={checksum}")
    return archive


def load_manifest(bundle_root: Path) -> dict[str, object]:
    manifest_path = bundle_root / MANIFEST_NAME
    try:
        info = manifest_path.lstat()
    except FileNotFoundError as exc:
        raise CandidateError("release artifact manifest is missing") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise CandidateError("release artifact manifest must be a regular file")
    if info.st_size <= 0 or info.st_size > MAX_MANIFEST_BYTES:
        raise CandidateError("release artifact manifest has invalid size")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise CandidateError("release artifact manifest is invalid JSON") from exc
    if not isinstance(manifest, dict):
        raise CandidateError("release artifact manifest must be an object")
    expected_keys = {
        "schema_version",
        "package_version",
        "source_commit",
        "platform",
        "architecture",
        "hub_agent_schema_version",
        "paired_handoff_commit",
        "install_profile",
        "files",
    }
    if set(manifest) != expected_keys:
        raise CandidateError("release artifact manifest has an unexpected schema")
    return manifest


def verify_bundle_dir(bundle_root: Path) -> dict[str, object]:
    if not bundle_root.is_dir() or bundle_root.is_symlink():
        raise CandidateError("bundle root must be a real directory")
    manifest = load_manifest(bundle_root)

    if manifest["schema_version"] != MANIFEST_SCHEMA_VERSION:
        raise CandidateError("unsupported release artifact manifest schema")
    package_version = validate_package_version(str(manifest["package_version"]))
    source_commit = validate_source_commit(str(manifest["source_commit"]))
    platform_name = normalized_platform(str(manifest["platform"]))
    architecture = normalized_architecture(str(manifest["architecture"]))
    schema_value = manifest["hub_agent_schema_version"]
    if not isinstance(schema_value, int) or isinstance(schema_value, bool) or not 1 <= schema_value <= 65535:
        raise CandidateError("manifest Hub/Agent schema version is invalid")
    paired = manifest["paired_handoff_commit"]
    profile = manifest["install_profile"]
    if platform_name == "macos":
        validate_source_commit(str(paired))
        if profile != INSTALL_PROFILE:
            raise CandidateError("macOS install profile is invalid")
    elif paired is not None or profile is not None:
        raise CandidateError("non-macOS candidate must not declare single-Mac pairing")
    if bundle_root.name != bundle_name(package_version, platform_name, architecture):
        raise CandidateError("bundle directory name does not match manifest identity")

    records = manifest["files"]
    if not isinstance(records, list):
        raise CandidateError("manifest files must be an array")
    expected = set(expected_artifact_paths(platform_name))
    if len(records) != len(expected):
        raise CandidateError("manifest file count does not match the platform allowlist")

    seen: set[str] = set()
    for record in records:
        if not isinstance(record, dict) or set(record) != {"path", "size", "sha256"}:
            raise CandidateError("manifest file record has an unexpected schema")
        relative = str(record["path"])
        validate_relative_path(relative)
        if relative in seen:
            raise CandidateError("manifest contains a duplicate file path")
        seen.add(relative)
        if relative not in expected:
            raise CandidateError(f"file is outside the platform allowlist: {relative}")

        size = record["size"]
        digest = str(record["sha256"])
        if not isinstance(size, int) or isinstance(size, bool) or size <= 0 or size > MAX_FILE_BYTES:
            raise CandidateError(f"manifest size is invalid: {relative}")
        if not SHA256_RE.fullmatch(digest):
            raise CandidateError(f"manifest SHA-256 is invalid: {relative}")

        path = bundle_root.joinpath(*PurePosixPath(relative).parts)
        try:
            info = path.lstat()
        except FileNotFoundError as exc:
            raise CandidateError(f"manifested file is missing: {relative}") from exc
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise CandidateError(f"manifested file is not regular: {relative}")
        if info.st_size != size:
            raise CandidateError(f"manifested file size differs: {relative}")
        if sha256_file(path) != digest:
            raise CandidateError(f"manifested file SHA-256 differs: {relative}")
        if relative.startswith("bin/") and platform_name != "windows" and os.name != "nt" and info.st_mode & 0o111 == 0:
            raise CandidateError(f"manifested Unix binary is not executable: {relative}")

    if seen != expected:
        raise CandidateError("manifest does not contain the exact platform allowlist")

    allowed_files = expected | {MANIFEST_NAME}
    for path in bundle_root.rglob("*"):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            raise CandidateError("bundle contains a symlink")
        if path.is_dir():
            continue
        if not stat.S_ISREG(info.st_mode):
            raise CandidateError("bundle contains a non-regular entry")
        relative = path.relative_to(bundle_root).as_posix()
        if relative not in allowed_files:
            raise CandidateError(f"bundle contains an unexpected file: {relative}")

    manifest["source_commit"] = source_commit
    return manifest


def verify_checksum(archive: Path, checksum_path: Path) -> None:
    try:
        info = checksum_path.lstat()
    except FileNotFoundError as exc:
        raise CandidateError("archive checksum file is missing") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode) or info.st_size > 4096:
        raise CandidateError("archive checksum file is invalid")
    line = checksum_path.read_text(encoding="ascii").strip()
    match = re.fullmatch(r"([0-9a-f]{64})  ([^/\\]+)", line)
    if not match or match.group(2) != archive.name:
        raise CandidateError("archive checksum record is malformed")
    if sha256_file(archive) != match.group(1):
        raise CandidateError("archive SHA-256 differs from checksum record")


def expected_root_from_archive(archive: Path) -> str:
    name = archive.name
    if name.endswith(".tar.gz"):
        return name[: -len(".tar.gz")]
    if name.endswith(".zip"):
        return name[: -len(".zip")]
    raise CandidateError("release candidate must be .tar.gz or .zip")


def validate_archive_member(name: str, expected_root: str) -> PurePosixPath:
    if not name or "\\" in name or ":" in name:
        raise CandidateError(f"unsafe archive member path: {name!r}")
    canonical = name.rstrip("/")
    path = validate_relative_path(canonical)
    if not path.parts or path.parts[0] != expected_root:
        raise CandidateError("archive member is outside the expected bundle root")
    return path


def ensure_archive_budget(count: int, size: int, total: int) -> None:
    if count > MAX_ARCHIVE_MEMBERS:
        raise CandidateError("archive contains too many members")
    if size < 0 or size > MAX_ARCHIVE_MEMBER_BYTES:
        raise CandidateError("archive member exceeds bounded size")
    if total > MAX_ARCHIVE_BYTES:
        raise CandidateError("archive expanded size exceeds bounded limit")


def extract_tar(archive: Path, extract_dir: Path, expected_root: str) -> None:
    total = 0
    with tarfile.open(archive, mode="r:gz") as tf:
        members = tf.getmembers()
        for index, member in enumerate(members, start=1):
            path = validate_archive_member(member.name, expected_root)
            total += member.size
            ensure_archive_budget(index, member.size, total)
            destination = extract_dir.joinpath(*path.parts)
            if member.isdir():
                destination.mkdir(parents=True, exist_ok=True)
                continue
            if not member.isreg():
                raise CandidateError("archive contains a non-regular entry")
            destination.parent.mkdir(parents=True, exist_ok=True)
            source = tf.extractfile(member)
            if source is None:
                raise CandidateError("archive regular file has no payload")
            with destination.open("xb") as output:
                shutil.copyfileobj(source, output, length=1024 * 1024)
            if os.name != "nt":
                destination.chmod(member.mode & 0o777)


def zip_entry_is_symlink(info: zipfile.ZipInfo) -> bool:
    mode = (info.external_attr >> 16) & 0o170000
    return mode == stat.S_IFLNK


def extract_zip(archive: Path, extract_dir: Path, expected_root: str) -> None:
    total = 0
    with zipfile.ZipFile(archive, mode="r") as zf:
        entries = zf.infolist()
        for index, info in enumerate(entries, start=1):
            path = validate_archive_member(info.filename, expected_root)
            total += info.file_size
            ensure_archive_budget(index, info.file_size, total)
            if zip_entry_is_symlink(info):
                raise CandidateError("archive contains a symlink")
            destination = extract_dir.joinpath(*path.parts)
            if info.is_dir():
                destination.mkdir(parents=True, exist_ok=True)
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            with zf.open(info, mode="r") as source, destination.open("xb") as output:
                shutil.copyfileobj(source, output, length=1024 * 1024)
            if os.name != "nt":
                mode = (info.external_attr >> 16) & 0o777
                if mode:
                    destination.chmod(mode)


def verify_archive(archive: Path, checksum: Path, extract_dir: Path) -> Path:
    try:
        info = archive.lstat()
    except FileNotFoundError as exc:
        raise CandidateError("release-candidate archive is missing") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise CandidateError("release-candidate archive must be a regular file")
    if info.st_size <= 0 or info.st_size > MAX_ARCHIVE_BYTES:
        raise CandidateError("release-candidate archive has invalid size")
    verify_checksum(archive, checksum)
    expected_root = expected_root_from_archive(archive)
    if extract_dir.exists():
        raise CandidateError("verification extract directory must not already exist")
    extract_dir.mkdir(parents=True, mode=0o755)
    try:
        if archive.name.endswith(".tar.gz"):
            extract_tar(archive, extract_dir, expected_root)
        else:
            extract_zip(archive, extract_dir, expected_root)
        bundle_root = extract_dir / expected_root
        verify_bundle_dir(bundle_root)
    except Exception:
        shutil.rmtree(extract_dir, ignore_errors=True)
        raise
    print(f"VERIFIED bundle={bundle_root}")
    return bundle_root


def current_platform() -> str:
    if sys.platform.startswith("linux"):
        return "linux"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform in {"win32", "cygwin"}:
        return "windows"
    raise CandidateError(f"unsupported smoke host: {sys.platform}")


def smoke_bundle(bundle_root: Path) -> None:
    bundle_root = bundle_root.resolve(strict=True)
    manifest = verify_bundle_dir(bundle_root)
    platform_name = str(manifest["platform"])
    if platform_name != current_platform():
        raise CandidateError("release-candidate smoke must run on its native platform")
    record_by_path = {
        str(record["path"]): record
        for record in manifest["files"]
        if isinstance(record, dict) and "path" in record
    }
    for relative in expected_binary_paths(platform_name):
        if relative not in record_by_path:
            raise CandidateError(f"packaged binary is missing from manifest: {relative}")
        binary = bundle_root.joinpath(*PurePosixPath(relative).parts)
        result = subprocess.run(
            [str(binary), "--help"],
            cwd=bundle_root,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=15,
            check=False,
        )
        if result.returncode != 0:
            raise CandidateError(f"packaged binary smoke failed: {binary.name}")
    print(f"SMOKE_OK bundle={bundle_root.name} host={host_platform.platform()}")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    build = commands.add_parser("build", help="build a bounded release-candidate archive")
    build.add_argument("--binary-dir", required=True)
    build.add_argument("--output-dir", required=True)
    build.add_argument("--package-version", required=True)
    build.add_argument("--source-commit", required=True)
    build.add_argument("--platform", required=True)
    build.add_argument("--architecture", required=True)
    build.add_argument("--hub-agent-schema-version", required=True, type=int)
    build.add_argument("--paired-handoff-commit")
    build.add_argument("--payload-dir")

    verify = commands.add_parser("verify", help="verify checksum, safely extract, and verify bundle")
    verify.add_argument("--archive", required=True)
    verify.add_argument("--checksum", required=True)
    verify.add_argument("--extract-dir", required=True)

    verify_dir = commands.add_parser("verify-dir", help="verify an already-extracted bundle")
    verify_dir.add_argument("--bundle-dir", required=True)

    smoke = commands.add_parser("smoke", help="run safe --help smoke from an extracted native bundle")
    smoke.add_argument("--bundle-dir", required=True)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "build":
            build_candidate(args)
        elif args.command == "verify":
            verify_archive(Path(args.archive), Path(args.checksum), Path(args.extract_dir))
        elif args.command == "verify-dir":
            verify_bundle_dir(Path(args.bundle_dir))
            print(f"VERIFIED_DIR bundle={args.bundle_dir}")
        elif args.command == "smoke":
            smoke_bundle(Path(args.bundle_dir))
        else:  # pragma: no cover
            raise CandidateError("unknown command")
    except (CandidateError, OSError, subprocess.SubprocessError) as exc:
        print(f"REFUSED reason={exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
