#!/usr/bin/env python3
"""Fail-closed first install from a verified single-Mac CUMG release artifact.

The installer never creates deployment identities or policy. The operator supplies a bounded
non-secret profile plus separately provisioned trust/secret files. Artifact identity is verified
before staging, signing, LaunchAgent activation, or mutation-authority initialization.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

# The installer imports verifier modules from inside the immutable release artifact. Inspection
# must not mutate that artifact by creating __pycache__ entries, because the closed manifest is
# verified both before and after extraction and unexpected files fail closed.
sys.dont_write_bytecode = True

SCRIPT_DIR = Path(__file__).resolve().parent


def _load(name: str, filename: str):
    path = SCRIPT_DIR / filename
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise InstallError(f"support module unavailable: {filename}")
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


class InstallError(RuntimeError):
    pass


release = _load("cumg_release_candidate", "v2_release_candidate.py")
payload = _load("cumg_artifact_payload", "v2_artifact_payload.py")

PROFILE_SCHEMA = 1
PROFILE_KEYS = {
    "schema_version",
    "device_id",
    "mcp_resource",
    "trusted_proxy_issuer",
    "trusted_proxy_subject",
    "expected_cua_version",
    "cua_command",
    "handoff_runtime_command",
    "codesign_fingerprint",
    "macos_team_id",
}
SECRET_FILES = (
    "hub.key",
    "grant.key",
    "device.key",
    "tls-server.key",
    "trusted-proxy.key",
)
TRUST_FILES = (
    "hub.pub",
    "grant.pub",
    "device.pub",
    "tls-root.der",
    "tls-server.pem",
    "grant-signer-policy.json",
    "northbound-policy.json",
)
RUNTIME_BINARIES = (
    "v2_hub",
    "v2_agent",
    "v2_maint",
    "v2_doctor",
    "v2_status",
    "v2_recover",
    "v2_recovery_enclave_helper",
    "v2_grant_signer",
)
LABELS = (
    "com.github.git-ksk.cumg-v2-grant-signer",
    "com.github.git-ksk.cumg-v2-hub",
    "com.github.git-ksk.cumg-v2-agent",
)
PLISTS = {
    "com.github.git-ksk.cumg-v2-grant-signer": "com.github.git-ksk.cumg-v2-grant-signer.plist",
    "com.github.git-ksk.cumg-v2-hub": "com.github.git-ksk.cumg-v2-hub.plist",
    "com.github.git-ksk.cumg-v2-agent": "com.github.git-ksk.cumg-v2-agent.plist",
}


def regular(path: Path, private: bool = False) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise InstallError(f"required provisioned file missing: {path.name}") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode) or info.st_size <= 0:
        raise InstallError(f"required provisioned file unsafe: {path.name}")
    if private and info.st_mode & 0o077:
        raise InstallError(f"secret file must not be group/world accessible: {path.name}")


def private_dir(path: Path) -> None:
    if path.exists():
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode) or info.st_mode & 0o077:
            raise InstallError(f"private directory unsafe: {path.name}")
    else:
        path.mkdir(parents=True, mode=0o700)
    path.chmod(0o700)


def load_profile(path: Path) -> dict[str, object]:
    regular(path)
    if path.stat().st_size > 32 * 1024:
        raise InstallError("profile is too large")
    try:
        profile = json.loads(path.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise InstallError("profile is invalid JSON") from exc
    if not isinstance(profile, dict) or set(profile) != PROFILE_KEYS or profile["schema_version"] != PROFILE_SCHEMA:
        raise InstallError("profile schema mismatch")
    for key in PROFILE_KEYS - {"schema_version"}:
        value = profile[key]
        if not isinstance(value, str) or not value or len(value) > 1024 or any(c in value for c in "\x00\r\n"):
            raise InstallError(f"profile value invalid: {key}")
    if not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", str(profile["device_id"])):
        raise InstallError("device_id is not a bounded safe identifier")
    parsed = urlsplit(str(profile["mcp_resource"]))
    if parsed.scheme != "https" or not parsed.netloc or parsed.fragment:
        raise InstallError("mcp_resource must be an absolute https URL without a fragment")
    if not re.fullmatch(r"[0-9A-Fa-f]{40}", str(profile["codesign_fingerprint"])):
        raise InstallError("codesign_fingerprint must be exact SHA-1 fingerprint")
    if not re.fullmatch(r"[A-Z0-9]{10}", str(profile["macos_team_id"])):
        raise InstallError("macos_team_id is invalid")
    for key in ("cua_command", "handoff_runtime_command"):
        p = Path(str(profile[key]))
        if not p.is_absolute() or not p.is_file() or p.is_symlink() or not os.access(p, os.X_OK):
            raise InstallError(f"{key} must be an existing absolute executable")
    return profile


def current_arch() -> str:
    value = platform.machine().lower()
    if value in {"arm64", "aarch64"}:
        return "arm64"
    if value in {"x86_64", "amd64"}:
        return "x64"
    return value


def verify_bundle(bundle: Path) -> dict[str, object]:
    manifest = release.verify_bundle_dir(bundle)
    if manifest["platform"] != "macos" or manifest["install_profile"] != release.INSTALL_PROFILE:
        raise InstallError("artifact is not the reviewed single-Mac install profile")
    if str(manifest["architecture"]).lower() != current_arch():
        raise InstallError("artifact architecture does not match this Mac")
    return manifest


def verify_provisioning(root: Path) -> None:
    if not root.is_dir() or root.is_symlink():
        raise InstallError("provisioning directory must be a real directory")
    secret_dir = root / "secrets"
    trust_dir = root / "trust"
    for directory, private in ((secret_dir, True), (trust_dir, False)):
        try:
            info = directory.lstat()
        except FileNotFoundError as exc:
            raise InstallError(f"provisioning directory missing: {directory.name}") from exc
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
            raise InstallError(f"provisioning directory unsafe: {directory.name}")
        if private and info.st_mode & 0o077:
            raise InstallError("secrets directory must be owner-private")
        if info.st_mode & 0o022:
            raise InstallError(f"provisioning directory must not be group/world writable: {directory.name}")
    for name in SECRET_FILES:
        regular(secret_dir / name, private=True)
    for name in TRUST_FILES:
        regular(trust_dir / name)


def sha(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def stable_sign(path: Path, identifier: str, fingerprint: str, team: str, runner=None) -> None:
    if runner is None:
        runner = subprocess.run
    expr = f'identifier "{identifier}" and anchor apple generic and certificate leaf[subject.OU] = "{team}"'
    requirement = f"=designated => {expr}"
    cmd = ["codesign", "--force", "--sign", fingerprint.upper(), "--identifier", identifier, "--requirements", requirement, str(path)]
    result = runner(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    if result.returncode != 0:
        raise InstallError(f"stable codesign failed: {path.name}")
    result = runner(["codesign", "--verify", "--strict", str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    if result.returncode != 0:
        raise InstallError(f"codesign verification failed: {path.name}")
    result = runner(["codesign", "-v", f"-R={expr}", str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    if result.returncode != 0:
        raise InstallError(f"codesign designated requirement mismatch: {path.name}")


def render_plists(bundle: Path, profile: dict[str, object], root: Path, run_root: Path, runtime: Path, out: Path) -> None:
    private_dir(out)
    replacements = {
        "@HOME@": str(Path.home()),
        "@ROOT@": str(root),
        "@RUN_ROOT@": str(run_root),
        "@BINARY_DIR@": str(root / "bin"),
        "@HANDOFF_CONTROL_SOCKET@": str(run_root / "handoff-control.sock"),
        "@HANDOFF_RUNTIME_COMMAND@": str(profile["handoff_runtime_command"]),
        "@HANDOFF_RUNTIME_SCRIPT@": str(runtime / "v2_handoff_runtime.mjs"),
        "@HANDOFF_RUNTIME_ENV_FILE@": str(root / "v2/handoff/managed-runtime.env"),
        "REPLACE_WITH_STABLE_DEVICE_ID": str(profile["device_id"]),
        "https://REPLACE_WITH_PUBLIC_RESOURCE/mcp": str(profile["mcp_resource"]),
        "REPLACE_WITH_ISSUER": str(profile["trusted_proxy_issuer"]),
        "REPLACE_WITH_SUBJECT": str(profile["trusted_proxy_subject"]),
    }
    for label, filename in PLISTS.items():
        src = bundle / "launchd" / filename
        regular(src)
        text = src.read_text(encoding="utf-8")
        for old, new in replacements.items():
            text = text.replace(old, new)
        # The reviewed template carries the current default Cua path/version; clean install makes the exact profile explicit.
        text = text.replace(str(Path.home() / ".local/bin/cua-driver"), str(profile["cua_command"]))
        text = text.replace("<string>0.19.3</string>", f'<string>{profile["expected_cua_version"]}</string>')
        if "@" in text or "REPLACE_" in text:
            raise InstallError(f"unresolved LaunchAgent placeholder: {filename}")
        data = plistlib.loads(text.encode("utf-8"))
        if data.get("Label") != label:
            raise InstallError(f"LaunchAgent label mismatch: {filename}")
        dst = out / filename
        # LaunchAgents contain only validated non-secret configuration and paths that point to
        # separately provisioned secret files; provisioning bytes never enter `text`. CodeQL's
        # sensitive-storage model treats the *_SECRET_FILE path fields as secret material.
        # codeql[py/clear-text-storage-sensitive-data]
        dst.write_text(text, encoding="utf-8")
        dst.chmod(0o600)


def write_runtime_manifest(root: Path, manifest: dict[str, object]) -> None:
    binary_dir = root / "bin"
    records = []
    for name in RUNTIME_BINARIES:
        p = binary_dir / name
        regular(p)
        records.append({"name": name, "sha256": sha(p)})
    data = {
        "schema_version": 3,
        "hub_agent_schema_version": manifest["hub_agent_schema_version"],
        "source_commit": manifest["source_commit"],
        "package_version": manifest["package_version"],
        "binaries": records,
    }
    tmp = root / f"runtime-manifest.json.new.{os.getpid()}"
    tmp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    tmp.chmod(0o600)
    os.replace(tmp, root / "runtime-manifest.json")


def copy_provisioning(src: Path, root: Path) -> None:
    for kind, names, mode in (("secrets", SECRET_FILES, 0o600), ("trust", TRUST_FILES, 0o600)):
        dst_dir = root / "v2" / kind
        private_dir(dst_dir)
        for name in names:
            src_file = src / kind / name
            dst = dst_dir / name
            if dst.exists() or dst.is_symlink():
                raise InstallError(f"refusing to overwrite provisioned file: {name}")
            shutil.copyfile(src_file, dst)
            dst.chmod(mode)


def install(args) -> None:
    if sys.platform != "darwin":
        raise InstallError("single-Mac artifact install requires macOS")
    for command in ("codesign", "launchctl"):
        if shutil.which(command) is None:
            raise InstallError(f"required command unavailable: {command}")
    bundle = Path(args.bundle_dir).resolve(strict=True)
    manifest = verify_bundle(bundle)
    profile = load_profile(Path(args.profile).resolve(strict=True))
    provisioning = Path(args.provisioning_dir).resolve(strict=True)
    verify_provisioning(provisioning)
    root = Path(args.install_root).expanduser().resolve()
    run_root = Path(args.run_root).expanduser().resolve()
    launch_agents = Path(args.launch_agent_dir).expanduser().resolve()
    if (root / "runtime-manifest.json").exists() or (root / "bin").exists():
        raise InstallError("existing installation detected; use the artifact-backed upgrade path")
    domain = f"gui/{os.getuid()}"
    for label in LABELS:
        result = subprocess.run(["launchctl", "print", f"{domain}/{label}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        if result.returncode == 0:
            raise InstallError(f"existing LaunchAgent is loaded: {label}")
    # Verify the inner paired payload before creating any install-root state.
    paired = str(manifest["paired_handoff_commit"])
    with tempfile.TemporaryDirectory(prefix="cumg-artifact-install-") as td:
        extracted = Path(td) / "handoff"
        runtime_source = payload.extract(argparse.Namespace(
            archive=str(bundle / "components/handoff-runtime.tar.gz"), output_dir=str(extracted),
            cumg_commit=str(manifest["source_commit"]), handoff_commit=paired,
        ))
        staging = Path(td) / "staging"
        shutil.copytree(bundle / "bin", staging / "bin")
        staged_runtime = staging / "runtime"
        shutil.copytree(runtime_source, staged_runtime)
        fp = str(profile["codesign_fingerprint"]); team = str(profile["macos_team_id"])
        stable_sign(staging / "bin/v2_agent", "com.github.git-ksk.cumg-v2-agent", fp, team)
        stable_sign(staging / "bin/v2_recover", "com.github.git-ksk.cumg-v2-recover", fp, team)
        stable_sign(staging / "bin/v2_recovery_enclave_helper", "com.github.git-ksk.cumg-v2-recovery-helper", fp, team)
        stable_sign(staged_runtime / "takeover-webrtc-host", "com.github.git-ksk.cumg-v2-handoff-webrtc-host", fp, team)
        # Signing changes the helper digest; refresh only the payload evidence manifest, never authority/state.
        payload_manifest = {
            "schema_version": payload.SCHEMA_VERSION,
            "cumg_source_commit": manifest["source_commit"],
            "handoff_source_commit": paired,
            "files": payload.manifest_records(staged_runtime),
        }
        (staged_runtime / payload.MANIFEST).write_text(json.dumps(payload_manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        payload.verify_tree(staged_runtime, str(manifest["source_commit"]), paired)
        if args.preflight_only:
            print(f"ARTIFACT_INSTALL_PREFLIGHT_OK source_commit={manifest['source_commit']} handoff_source_commit={paired}")
            return
        private_dir(root); private_dir(run_root)
        for rel in ("bin", "v2", "v2/state", "v2/state/hub", "v2/state/agent", "v2/handoff", "rollback"):
            private_dir(root / rel)
        copy_provisioning(provisioning, root)
        runtime_name = f"runtime-{str(manifest['source_commit'])[:12]}-{paired[:12]}"
        runtime = root / "v2/handoff" / runtime_name
        if runtime.exists():
            raise InstallError("target Handoff runtime generation already exists")
        shutil.copytree(staged_runtime, runtime)
        for p in runtime.rglob("*"):
            if p.is_dir(): p.chmod(0o700)
            elif p.is_file(): p.chmod(0o700 if p.name in {"v2_handoff_runtime.mjs", "takeover-webrtc-host"} else 0o600)
        env_file = root / "v2/handoff/managed-runtime.env"
        env_file.write_text(
            f"CUMG_V2_HANDOFF_ROOT={runtime / 'handoff-root'}\n"
            f"CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE={runtime / 'takeover-webrtc-host'}\n",
            encoding="utf-8",
        ); env_file.chmod(0o600)
        for source in (staging / "bin").iterdir():
            target = root / "bin" / source.name
            shutil.copyfile(source, target); target.chmod(0o700)
        copy_plists = Path(td) / "plists"
        render_plists(bundle, profile, root, run_root, runtime, copy_plists)
        if launch_agents.exists():
            info = launch_agents.lstat()
            if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
                raise InstallError("LaunchAgents directory is unsafe")
        else:
            launch_agents.mkdir(parents=True, mode=0o755)
        for filename in PLISTS.values():
            target = launch_agents / filename
            if target.exists() or target.is_symlink():
                raise InstallError(f"refusing to overwrite LaunchAgent: {filename}")
            shutil.copyfile(copy_plists / filename, target); target.chmod(0o600)
        result = subprocess.run([str(root / "bin/v2_maint"), "mutation-authority-init", "--authority-dir", str(root / "mutation-authority"), "--owner", "v2"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        if result.returncode != 0:
            raise InstallError("mutation authority initialization failed")
        write_runtime_manifest(root, manifest)
    started: list[str] = []
    try:
        for label in LABELS:
            plist = launch_agents / PLISTS[label]
            result = subprocess.run(["launchctl", "bootstrap", domain, str(plist)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
            if result.returncode != 0:
                raise InstallError(f"LaunchAgent startup failed: {label}")
            started.append(label); time.sleep(1)
        doctor_cmd = [str(root / "bin/v2_doctor"), "--install-root", str(root), "--run-root", str(run_root), "--expected-cua-version", str(profile["expected_cua_version"]), "--cua-command", str(profile["cua_command"]), "--handoff-control-socket", str(run_root / "handoff-control.sock"), "--json"]
        doctor = None
        for _ in range(15):
            result = subprocess.run(doctor_cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, check=False)
            if result.returncode == 0:
                doctor = json.loads(result.stdout); break
            time.sleep(1)
        if not isinstance(doctor, dict) or doctor.get("overall") != "healthy":
            raise InstallError("installed v2_doctor did not become healthy")
        status = subprocess.run([str(root / "bin/v2_status"), "--install-root", str(root), "--run-root", str(run_root), "--json"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, check=False)
        if status.returncode != 0:
            raise InstallError("installed v2_status returned non-zero")
        report = json.loads(status.stdout)
        if report.get("overall") != "healthy" or report.get("next_action") != "none":
            raise InstallError("installed v2_status did not report healthy")
    except Exception:
        for label in reversed(started):
            subprocess.run(["launchctl", "bootout", f"{domain}/{label}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        raise
    print(f"ARTIFACT_INSTALL_OK source_commit={manifest['source_commit']} handoff_source_commit={manifest['paired_handoff_commit']} runtime_generation={runtime_name}")


def inspect(args) -> None:
    manifest = verify_bundle(Path(args.bundle_dir).resolve(strict=True))
    print(json.dumps({
        "schema_version": 1,
        "package_version": manifest["package_version"],
        "source_commit": manifest["source_commit"],
        "hub_agent_schema_version": manifest["hub_agent_schema_version"],
        "paired_handoff_commit": manifest["paired_handoff_commit"],
        "install_profile": manifest["install_profile"],
    }, indent=2))


def parser() -> argparse.ArgumentParser:
    p=argparse.ArgumentParser(description=__doc__); sub=p.add_subparsers(dest="command",required=True)
    i=sub.add_parser("inspect", help="verify and print bounded artifact identity"); i.add_argument("--bundle-dir",required=True)
    x=sub.add_parser("install", help="perform fail-closed first install from verified artifact")
    x.add_argument("--bundle-dir",required=True); x.add_argument("--profile",required=True); x.add_argument("--provisioning-dir",required=True)
    x.add_argument("--install-root",default=str(Path.home()/"Library/Application Support/computer-use-mcp-gateway"))
    x.add_argument("--run-root",default=str(Path.home()/"Library/Caches/cumg-v2"))
    x.add_argument("--launch-agent-dir",default=str(Path.home()/"Library/LaunchAgents")); x.add_argument("--preflight-only",action="store_true")
    return p


def main() -> int:
    try:
        args=parser().parse_args(); inspect(args) if args.command=="inspect" else install(args); return 0
    except (InstallError, release.CandidateError, payload.PayloadError, OSError, subprocess.SubprocessError, json.JSONDecodeError, plistlib.InvalidFileException) as exc:
        print(f"REFUSED reason={exc}", file=sys.stderr); return 2
if __name__ == "__main__": raise SystemExit(main())
