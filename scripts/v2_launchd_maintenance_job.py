#!/usr/bin/env python3
"""Bounded one-shot launchd runner and stale-job guard for single-Mac CUMG maintenance.

The runner deliberately avoids ``launchctl submit``.  It writes an owner-private plist with
RunAtLoad=true and KeepAlive=false, bootstraps it into the current GUI domain, verifies that the
job runs no more than once, then always boots it out and removes the temporary plist.

Only a closed allowlist of non-secret environment values required by the reviewed upgrade helper
is forwarded.  Program arguments, environment contents, and command output are never printed by
inspection/cleanup paths.
"""

from __future__ import annotations

import argparse
import dataclasses
import os
import pathlib
import plistlib
import re
import secrets
import stat
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence

NEW_PREFIX = "com.github.git-ksk.cumg-v2-maintenance."
LEGACY_PREFIXES = (
    "com.git-ksk.cumg-v2-upgrade-",
    "com.github.git-ksk.cumg-v2-upgrade-",
)
MAX_MAINTENANCE_JOBS = 64
DEFAULT_TIMEOUT_SECS = 15 * 60
POLL_INTERVAL_SECS = 0.1
POST_EXIT_STABILITY_SECS = 0.5
LABEL_RE = re.compile(r"^[A-Za-z0-9._-]{1,180}$")
DOMAIN_RE = re.compile(r"^gui/[0-9]+$")

SAFE_UPGRADE_ENV_KEYS = (
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "DEVELOPER_DIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "CUMG_V2_INSTALL_ROOT",
    "CUMG_V2_RUN_ROOT",
    "CUMG_V2_HUB_LABEL",
    "CUMG_V2_AGENT_LABEL",
    "CUMG_V2_SIGNER_LABEL",
    "CUMG_V2_EXTERNAL_SIGNER",
    "CUMG_V2_EXPECTED_CUA_VERSION",
    "CUMG_V2_MACOS_CODESIGN_FINGERPRINT",
    "CUMG_V2_MACOS_CODESIGN_IDENTITY",
    "CUMG_V2_MACOS_TEAM_ID",
    "CUMG_V2_HANDOFF_SOURCE_ROOT",
    "CUMG_V2_EXPECTED_HANDOFF_COMMIT",
)

Runner = Callable[..., subprocess.CompletedProcess[str]]


class MaintenanceError(RuntimeError):
    def __init__(self, reason: str, details: str = "") -> None:
        self.reason = reason
        self.details = details
        super().__init__(f"{reason}{(' ' + details) if details else ''}")


@dataclasses.dataclass(frozen=True)
class MaintenanceJobStatus:
    label: str
    state: str
    runs: int
    last_exit_code: int | None
    pid: int | None

    @property
    def running(self) -> bool:
        return self.state == "running" and self.pid is not None


def is_maintenance_label(label: str) -> bool:
    if not LABEL_RE.fullmatch(label):
        return False
    return label.startswith(NEW_PREFIX) or label.startswith(LEGACY_PREFIXES)


def _run(runner: Runner, argv: Sequence[str]) -> subprocess.CompletedProcess[str]:
    return runner(
        list(argv),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )


def _validate_domain(domain: str) -> None:
    if not DOMAIN_RE.fullmatch(domain):
        raise MaintenanceError("invalid_launchd_domain")


def _validate_label(label: str) -> None:
    if not is_maintenance_label(label):
        raise MaintenanceError("invalid_maintenance_label")


def labels_from_domain_output(output: str) -> tuple[str, ...]:
    found: set[str] = set()
    for token in re.findall(r"[A-Za-z0-9._-]+", output):
        if is_maintenance_label(token):
            found.add(token)
    if len(found) > MAX_MAINTENANCE_JOBS:
        raise MaintenanceError("too_many_maintenance_jobs")
    return tuple(sorted(found))


def parse_job_status(label: str, output: str) -> MaintenanceJobStatus:
    _validate_label(label)
    state = "unknown"
    runs = 0
    last_exit_code: int | None = None
    pid: int | None = None
    for raw in output.splitlines():
        line = raw.strip()
        if line.startswith("state = ") and state == "unknown":
            state = line.removeprefix("state = ").strip()
        elif line.startswith("runs = "):
            try:
                runs = int(line.removeprefix("runs = ").strip())
            except ValueError:
                raise MaintenanceError("invalid_maintenance_job_status") from None
        elif line.startswith("last exit code = "):
            value = line.removeprefix("last exit code = ").strip()
            if value == "(never exited)":
                last_exit_code = None
            else:
                try:
                    last_exit_code = int(value)
                except ValueError:
                    raise MaintenanceError("invalid_maintenance_job_status") from None
        elif line.startswith("pid = "):
            try:
                pid = int(line.removeprefix("pid = ").strip())
            except ValueError:
                raise MaintenanceError("invalid_maintenance_job_status") from None
    if runs < 0 or pid is not None and pid <= 0:
        raise MaintenanceError("invalid_maintenance_job_status")
    return MaintenanceJobStatus(
        label=label,
        state=state,
        runs=runs,
        last_exit_code=last_exit_code,
        pid=pid,
    )


def inspect_maintenance_jobs(
    *,
    domain: str,
    launchctl: str,
    exclude_label: str | None = None,
    runner: Runner = subprocess.run,
) -> tuple[MaintenanceJobStatus, ...]:
    _validate_domain(domain)
    if exclude_label is not None:
        _validate_label(exclude_label)
    domain_result = _run(runner, [launchctl, "print", domain])
    if domain_result.returncode != 0:
        raise MaintenanceError("launchd_domain_unreadable")
    statuses: list[MaintenanceJobStatus] = []
    for label in labels_from_domain_output(domain_result.stdout):
        if label == exclude_label:
            continue
        result = _run(runner, [launchctl, "print", f"{domain}/{label}"])
        if result.returncode != 0:
            # Domain enumeration can race a job being explicitly booted out. Treat it as gone.
            continue
        statuses.append(parse_job_status(label, result.stdout))
    return tuple(statuses)


def assert_no_stale_jobs(
    *,
    domain: str,
    launchctl: str,
    exclude_label: str | None = None,
    runner: Runner = subprocess.run,
) -> None:
    jobs = inspect_maintenance_jobs(
        domain=domain,
        launchctl=launchctl,
        exclude_label=exclude_label,
        runner=runner,
    )
    if jobs:
        active = sum(job.running for job in jobs)
        raise MaintenanceError(
            "stale_maintenance_jobs",
            f"count={len(jobs)} active={active}",
        )


def _reject_existing_symlink_components(path: pathlib.Path) -> None:
    current = pathlib.Path(path.anchor)
    for part in path.parts[1:]:
        current = current / part
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(metadata.st_mode) and metadata.st_uid != 0:
            raise MaintenanceError("unsafe_maintenance_job_dir")


def _secure_job_dir(path: pathlib.Path) -> pathlib.Path:
    path = path.expanduser()
    if not path.is_absolute():
        path = pathlib.Path.cwd() / path
    _reject_existing_symlink_components(path)
    existed = path.exists()
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    metadata = path.lstat()
    if (
        stat.S_ISLNK(metadata.st_mode)
        or not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_mode & 0o077
    ):
        raise MaintenanceError("unsafe_maintenance_job_dir")
    if not existed:
        os.chmod(path, 0o700)
        metadata = path.lstat()
        if metadata.st_mode & 0o077:
            raise MaintenanceError("unsafe_maintenance_job_dir")
    return path


def cleanup_stale_jobs(
    *,
    domain: str,
    launchctl: str,
    job_dir: pathlib.Path,
    runner: Runner = subprocess.run,
) -> int:
    jobs = inspect_maintenance_jobs(domain=domain, launchctl=launchctl, runner=runner)
    active = [job for job in jobs if job.running]
    if active:
        raise MaintenanceError(
            "active_maintenance_job",
            f"count={len(active)}",
        )
    root = _secure_job_dir(job_dir)
    cleaned = 0
    for job in jobs:
        target = f"{domain}/{job.label}"
        result = _run(runner, [launchctl, "bootout", target])
        if result.returncode != 0:
            verify = _run(runner, [launchctl, "print", target])
            if verify.returncode == 0:
                raise MaintenanceError("maintenance_job_bootout_failed")
        plist_path = root / f"{job.label}.plist"
        if plist_path.exists() or plist_path.is_symlink():
            metadata = plist_path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                raise MaintenanceError("unsafe_maintenance_plist")
            plist_path.unlink()
        cleaned += 1
    return cleaned


def safe_upgrade_environment(
    source: Mapping[str, str],
    *,
    job_label: str,
) -> dict[str, str]:
    _validate_label(job_label)
    environment = {
        key: source[key]
        for key in SAFE_UPGRADE_ENV_KEYS
        if key in source and source[key] != ""
    }
    environment["CUMG_V2_MAINTENANCE_JOB_LABEL"] = job_label
    return environment


def build_one_shot_plist(
    *,
    label: str,
    program_arguments: Sequence[str],
    working_directory: pathlib.Path,
    environment: Mapping[str, str],
) -> bytes:
    _validate_label(label)
    if not program_arguments or any(not argument for argument in program_arguments):
        raise MaintenanceError("invalid_maintenance_program")
    if not working_directory.is_absolute():
        raise MaintenanceError("invalid_maintenance_working_directory")
    payload = {
        "Label": label,
        "ProgramArguments": list(program_arguments),
        "WorkingDirectory": str(working_directory),
        "RunAtLoad": True,
        "KeepAlive": False,
        "ProcessType": "Background",
        "EnvironmentVariables": dict(environment),
        "StandardOutPath": "/dev/null",
        "StandardErrorPath": "/dev/null",
    }
    return plistlib.dumps(payload, fmt=plistlib.FMT_XML, sort_keys=True)


def _write_private_plist(path: pathlib.Path, payload: bytes) -> None:
    if path.exists() or path.is_symlink():
        raise MaintenanceError("maintenance_plist_exists")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags, 0o600)
    try:
        metadata = os.fstat(fd)
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_nlink != 1
            or metadata.st_mode & 0o077
        ):
            raise MaintenanceError("unsafe_maintenance_plist")
        with os.fdopen(fd, "wb") as handle:
            fd = -1
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
    except Exception:
        if fd >= 0:
            os.close(fd)
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        raise


def wait_for_one_shot_completion(
    *,
    domain: str,
    label: str,
    launchctl: str,
    timeout_secs: float,
    runner: Runner = subprocess.run,
    sleep: Callable[[float], None] = time.sleep,
) -> int:
    _validate_domain(domain)
    _validate_label(label)
    target = f"{domain}/{label}"
    deadline = time.monotonic() + timeout_secs
    observed_run = False
    last: MaintenanceJobStatus | None = None
    while time.monotonic() < deadline:
        result = _run(runner, [launchctl, "print", target])
        if result.returncode != 0:
            raise MaintenanceError("maintenance_job_disappeared")
        last = parse_job_status(label, result.stdout)
        if last.runs > 1:
            raise MaintenanceError("automatic_maintenance_relaunch_detected")
        observed_run = observed_run or last.runs == 1 or last.running
        # launchd can briefly report state=xpcproxy with runs=1 and a pid before the
        # target process reaches state=running. Only the exact terminal state proves
        # that the one-shot has completed; treating any non-running state as terminal
        # creates a false relaunch signal for long-running maintenance jobs.
        if observed_run and last.state == "not running" and last.runs == 1:
            break
        sleep(POLL_INTERVAL_SECS)
    else:
        raise MaintenanceError("maintenance_job_timeout")

    sleep(POST_EXIT_STABILITY_SECS)
    stable = _run(runner, [launchctl, "print", target])
    if stable.returncode != 0:
        raise MaintenanceError("maintenance_job_disappeared")
    final = parse_job_status(label, stable.stdout)
    if final.state != "not running" or final.runs != 1:
        raise MaintenanceError("automatic_maintenance_relaunch_detected")
    if final.last_exit_code is None:
        raise MaintenanceError("maintenance_exit_status_missing")
    return final.last_exit_code


def run_upgrade_one_shot(
    *,
    repo_root: pathlib.Path,
    domain: str,
    launchctl: str,
    job_dir: pathlib.Path,
    environment: Mapping[str, str],
    timeout_secs: float = DEFAULT_TIMEOUT_SECS,
    runner: Runner = subprocess.run,
    sleep: Callable[[float], None] = time.sleep,
    now: Callable[[], float] = time.time,
    token_hex: Callable[[int], str] = secrets.token_hex,
) -> int:
    _validate_domain(domain)
    assert_no_stale_jobs(domain=domain, launchctl=launchctl, runner=runner)
    repo_root = repo_root.resolve(strict=True)
    upgrade = repo_root / "scripts/v2-single-mac-upgrade.sh"
    if not upgrade.is_file() or upgrade.is_symlink():
        raise MaintenanceError("upgrade_helper_missing_or_unsafe")
    root = _secure_job_dir(job_dir)
    label = f"{NEW_PREFIX}upgrade.{int(now())}.{os.getpid()}.{token_hex(4)}"
    _validate_label(label)
    plist_path = root / f"{label}.plist"
    payload = build_one_shot_plist(
        label=label,
        program_arguments=["/bin/bash", str(upgrade)],
        working_directory=repo_root,
        environment=safe_upgrade_environment(environment, job_label=label),
    )
    _write_private_plist(plist_path, payload)
    target = f"{domain}/{label}"
    bootstrapped = False
    job_exit: int | None = None
    cleanup_error: MaintenanceError | None = None
    try:
        result = _run(runner, [launchctl, "bootstrap", domain, str(plist_path)])
        if result.returncode != 0:
            raise MaintenanceError("maintenance_job_bootstrap_failed")
        bootstrapped = True
        job_exit = wait_for_one_shot_completion(
            domain=domain,
            label=label,
            launchctl=launchctl,
            timeout_secs=timeout_secs,
            runner=runner,
            sleep=sleep,
        )
    finally:
        if bootstrapped:
            result = _run(runner, [launchctl, "bootout", target])
            if result.returncode != 0:
                verify = _run(runner, [launchctl, "print", target])
                if verify.returncode == 0:
                    cleanup_error = MaintenanceError("maintenance_job_bootout_failed")
        try:
            if plist_path.exists() or plist_path.is_symlink():
                metadata = plist_path.lstat()
                if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                    cleanup_error = cleanup_error or MaintenanceError("unsafe_maintenance_plist")
                else:
                    plist_path.unlink()
        except OSError:
            cleanup_error = cleanup_error or MaintenanceError("maintenance_plist_cleanup_failed")
    if cleanup_error is not None:
        raise cleanup_error
    if job_exit is None:
        raise MaintenanceError("maintenance_job_no_exit")
    return job_exit


def _default_domain() -> str:
    return f"gui/{os.getuid()}"


def _default_job_dir() -> pathlib.Path:
    run_root = pathlib.Path(
        os.environ.get("CUMG_V2_RUN_ROOT", pathlib.Path.home() / "Library/Caches/cumg-v2")
    )
    return run_root / "maintenance-jobs"


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--launchctl", default="/bin/launchctl")
    parser.add_argument("--domain", default=_default_domain())
    parser.add_argument("--job-dir", type=pathlib.Path, default=_default_job_dir())
    sub = parser.add_subparsers(dest="command", required=True)

    inspect = sub.add_parser("inspect")
    inspect.add_argument("--exclude-label")

    clear = sub.add_parser("assert-clear")
    clear.add_argument("--exclude-label")

    sub.add_parser("cleanup-stale")

    run = sub.add_parser("run-upgrade")
    run.add_argument(
        "--repo-root",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parents[1],
    )
    run.add_argument("--timeout-secs", type=float, default=DEFAULT_TIMEOUT_SECS)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "inspect":
            jobs = inspect_maintenance_jobs(
                domain=args.domain,
                launchctl=args.launchctl,
                exclude_label=args.exclude_label,
            )
            for job in jobs:
                exit_code = "none" if job.last_exit_code is None else str(job.last_exit_code)
                print(
                    f"MAINTENANCE_JOB label={job.label} state={job.state} "
                    f"runs={job.runs} last_exit_code={exit_code}"
                )
            print(
                f"MAINTENANCE_JOBS count={len(jobs)} "
                f"active={sum(job.running for job in jobs)}"
            )
            return 0
        if args.command == "assert-clear":
            assert_no_stale_jobs(
                domain=args.domain,
                launchctl=args.launchctl,
                exclude_label=args.exclude_label,
            )
            print("MAINTENANCE_JOBS_OK count=0")
            return 0
        if args.command == "cleanup-stale":
            cleaned = cleanup_stale_jobs(
                domain=args.domain,
                launchctl=args.launchctl,
                job_dir=args.job_dir,
            )
            print(f"MAINTENANCE_JOBS_CLEANUP_OK cleaned={cleaned}")
            return 0
        if args.command == "run-upgrade":
            exit_code = run_upgrade_one_shot(
                repo_root=args.repo_root,
                domain=args.domain,
                launchctl=args.launchctl,
                job_dir=args.job_dir,
                environment=os.environ,
                timeout_secs=args.timeout_secs,
            )
            print(f"MAINTENANCE_JOB_COMPLETE exit_code={exit_code}")
            return exit_code
    except MaintenanceError as error:
        suffix = f" {error.details}" if error.details else ""
        print(f"REFUSED reason={error.reason}{suffix}", file=sys.stderr)
        return 2
    raise AssertionError("unreachable")


if __name__ == "__main__":
    raise SystemExit(main())
