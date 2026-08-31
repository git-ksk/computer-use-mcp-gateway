#!/usr/bin/env python3
"""Build/verify the bounded self-contained macOS Handoff payload for CUMG artifacts."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import tarfile
import tempfile

SCHEMA_VERSION = 1
MANIFEST = "runtime-generation-manifest.json"
MAX_FILES = 8192
MAX_FILE = 128 * 1024 * 1024
MAX_TOTAL = 512 * 1024 * 1024
MAX_ARCHIVE_MEMBERS = 12288
REQUIRED = {
    "v2_handoff_runtime.mjs",
    "takeover-webrtc-host",
    "handoff-root/dist/index.js",
    "handoff-root/package.json",
    "handoff-root/package-lock.json",
}

class PayloadError(RuntimeError):
    pass

def sha(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()

def safe_rel(value: str) -> PurePosixPath:
    if not value or "\\" in value or ":" in value:
        raise PayloadError("unsafe payload path")
    p = PurePosixPath(value)
    if p.is_absolute() or any(x in {"", ".", ".."} for x in p.parts) or p.as_posix() != value:
        raise PayloadError("unsafe payload path")
    return p

def iter_regular(root: Path):
    count = total = 0
    for p in sorted(root.rglob("*"), key=lambda x: x.as_posix()):
        info = p.lstat()
        if stat.S_ISLNK(info.st_mode):
            raise PayloadError(f"payload contains symlink: {p.name}")
        if p.is_dir():
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_size <= 0 or info.st_size > MAX_FILE:
            raise PayloadError(f"unsafe payload file: {p.name}")
        count += 1
        total += info.st_size
        if count > MAX_FILES or total > MAX_TOTAL:
            raise PayloadError("payload exceeds bounded budget")
        yield p

def manifest_records(root: Path) -> list[dict[str, object]]:
    records = []
    for p in iter_regular(root):
        if p.name == MANIFEST:
            continue
        records.append({"path": p.relative_to(root).as_posix(), "size": p.stat().st_size, "sha256": sha(p)})
    return records

def verify_tree(root: Path, cumg: str | None = None, handoff: str | None = None) -> dict:
    if not root.is_dir() or root.is_symlink():
        raise PayloadError("payload root must be a real directory")
    mp = root / MANIFEST
    if not mp.is_file() or mp.is_symlink() or mp.stat().st_size > 1024 * 1024:
        raise PayloadError("payload manifest missing or unsafe")
    try:
        data = json.loads(mp.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PayloadError("payload manifest invalid") from exc
    if set(data) != {"schema_version", "cumg_source_commit", "handoff_source_commit", "files"}:
        raise PayloadError("payload manifest schema mismatch")
    if data["schema_version"] != SCHEMA_VERSION:
        raise PayloadError("payload schema unsupported")
    for key in ("cumg_source_commit", "handoff_source_commit"):
        value = data[key]
        if not isinstance(value, str) or len(value) != 40 or any(c not in "0123456789abcdef" for c in value):
            raise PayloadError("payload commit identity invalid")
    if cumg is not None and data["cumg_source_commit"] != cumg:
        raise PayloadError("CUMG payload commit mismatch")
    if handoff is not None and data["handoff_source_commit"] != handoff:
        raise PayloadError("Handoff payload commit mismatch")
    records = data["files"]
    if not isinstance(records, list) or len(records) > MAX_FILES:
        raise PayloadError("payload file records invalid")
    actual = {p.relative_to(root).as_posix(): p for p in iter_regular(root) if p.name != MANIFEST}
    seen = set()
    for record in records:
        if not isinstance(record, dict) or set(record) != {"path", "size", "sha256"}:
            raise PayloadError("payload record schema mismatch")
        rel = str(record["path"]); safe_rel(rel)
        if rel in seen or rel not in actual:
            raise PayloadError("payload record missing or duplicate")
        seen.add(rel)
        p = actual[rel]
        if record["size"] != p.stat().st_size or record["sha256"] != sha(p):
            raise PayloadError("payload digest mismatch")
    if seen != set(actual):
        raise PayloadError("payload contains unmanifested files")
    if not REQUIRED.issubset(actual):
        raise PayloadError("payload required files missing")
    if not any(x.startswith("handoff-root/node_modules/werift/") for x in actual):
        raise PayloadError("payload production dependencies missing")
    return data

def remove_symlink_shims(node_modules: Path) -> None:
    bindir = node_modules / ".bin"
    if bindir.exists() or bindir.is_symlink():
        if bindir.is_symlink() or not bindir.is_dir():
            raise PayloadError("unsafe node_modules/.bin")
        shutil.rmtree(bindir)

def build(args) -> Path:
    source = Path(args.handoff_source).resolve(strict=True)
    host = Path(args.webrtc_host).resolve(strict=True)
    runtime = Path(args.runtime_script).resolve(strict=True)
    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)
    archive = out / "handoff-runtime.tar.gz"
    if archive.exists():
        raise PayloadError("refusing to overwrite payload")
    for rel in ("dist/index.js", "package.json", "package-lock.json", "node_modules/werift/package.json"):
        p = source / rel
        if not p.is_file() or p.is_symlink():
            raise PayloadError(f"Handoff input missing or unsafe: {rel}")
    if not host.is_file() or host.is_symlink() or host.stat().st_mode & 0o111 == 0:
        raise PayloadError("WebRTC host missing or not executable")
    if not runtime.is_file() or runtime.is_symlink():
        raise PayloadError("runtime script missing")
    with tempfile.TemporaryDirectory(prefix="cumg-handoff-payload-") as td:
        root = Path(td) / "handoff-runtime"
        hr = root / "handoff-root"
        hr.mkdir(parents=True)
        shutil.copytree(source / "dist", hr / "dist")
        shutil.copy2(source / "package.json", hr / "package.json")
        shutil.copy2(source / "package-lock.json", hr / "package-lock.json")
        shutil.copytree(source / "node_modules", hr / "node_modules", symlinks=True)
        remove_symlink_shims(hr / "node_modules")
        shutil.copy2(runtime, root / "v2_handoff_runtime.mjs")
        shutil.copy2(host, root / "takeover-webrtc-host")
        (root / "v2_handoff_runtime.mjs").chmod(0o700)
        (root / "takeover-webrtc-host").chmod(0o700)
        records = manifest_records(root)
        data = {
            "schema_version": SCHEMA_VERSION,
            "cumg_source_commit": args.cumg_commit,
            "handoff_source_commit": args.handoff_commit,
            "files": records,
        }
        (root / MANIFEST).write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        verify_tree(root, args.cumg_commit, args.handoff_commit)
        with archive.open("xb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as gz:
                with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tf:
                    for p in [root] + sorted(root.rglob("*"), key=lambda x: x.as_posix()):
                        rel = p.relative_to(root.parent).as_posix()
                        info = tarfile.TarInfo(rel)
                        info.uid = info.gid = 0; info.uname = info.gname = ""; info.mtime = 0
                        if p.is_dir():
                            info.type = tarfile.DIRTYPE; info.mode = 0o755; tf.addfile(info); continue
                        data_bytes = p.read_bytes(); info.size = len(data_bytes)
                        info.mode = 0o755 if p.name in {"v2_handoff_runtime.mjs", "takeover-webrtc-host"} else 0o644
                        import io
                        tf.addfile(info, io.BytesIO(data_bytes))
    print(f"PAYLOAD_BUILT archive={archive}")
    return archive

def extract(args) -> Path:
    archive = Path(args.archive).resolve(strict=True)
    dest = Path(args.output_dir)
    if dest.exists():
        raise PayloadError("payload output already exists")
    dest.mkdir(parents=True, mode=0o700)
    total = count = 0
    try:
        with tarfile.open(archive, "r:gz") as tf:
            for member in tf.getmembers():
                rel = safe_rel(member.name.rstrip("/"))
                if not rel.parts or rel.parts[0] != "handoff-runtime":
                    raise PayloadError("payload archive root mismatch")
                count += 1; total += member.size
                if count > MAX_ARCHIVE_MEMBERS or member.size > MAX_FILE or total > MAX_TOTAL:
                    raise PayloadError("payload archive budget exceeded")
                target = dest.joinpath(*rel.parts)
                if member.isdir():
                    target.mkdir(parents=True, exist_ok=True); continue
                if not member.isreg():
                    raise PayloadError("payload archive contains non-regular entry")
                target.parent.mkdir(parents=True, exist_ok=True)
                src = tf.extractfile(member)
                if src is None: raise PayloadError("payload entry unreadable")
                with target.open("xb") as f: shutil.copyfileobj(src, f, length=1024*1024)
                target.chmod(member.mode & 0o777)
        root = dest / "handoff-runtime"
        verify_tree(root, args.cumg_commit, args.handoff_commit)
        print(f"PAYLOAD_VERIFIED root={root}")
        return root
    except Exception:
        shutil.rmtree(dest, ignore_errors=True)
        raise

def parser():
    p=argparse.ArgumentParser(description=__doc__); s=p.add_subparsers(dest="command", required=True)
    b=s.add_parser("build"); b.add_argument("--handoff-source",required=True); b.add_argument("--webrtc-host",required=True); b.add_argument("--runtime-script",required=True); b.add_argument("--output-dir",required=True); b.add_argument("--cumg-commit",required=True); b.add_argument("--handoff-commit",required=True)
    e=s.add_parser("extract"); e.add_argument("--archive",required=True); e.add_argument("--output-dir",required=True); e.add_argument("--cumg-commit",required=True); e.add_argument("--handoff-commit",required=True)
    return p

def main():
    try:
        args=parser().parse_args(); build(args) if args.command=="build" else extract(args); return 0
    except (PayloadError,OSError,tarfile.TarError) as exc:
        print(f"REFUSED reason={exc}", file=os.sys.stderr); return 2
if __name__=="__main__": raise SystemExit(main())
