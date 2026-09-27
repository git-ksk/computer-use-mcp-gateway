#!/usr/bin/env python3
"""Validate a Cloud Run secret-volume graph without printing secret metadata."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
from typing import Any

EXPECTED_SECRET_MOUNTS = {
    "oidc_jwt": 6,
    "oauth_introspection": 7,
}


def _template_spec(document: dict[str, Any]) -> dict[str, Any]:
    spec = document.get("spec")
    if not isinstance(spec, dict):
        raise ValueError("manifest_spec_missing")
    template = spec.get("template")
    if isinstance(template, dict):
        nested = template.get("spec")
        if not isinstance(nested, dict):
            raise ValueError("manifest_template_spec_missing")
        return nested
    return spec


def _version_is_pinned(secret: dict[str, Any]) -> bool:
    items = secret.get("items")
    if not isinstance(items, list) or not items:
        return False
    for item in items:
        if not isinstance(item, dict):
            return False
        key = item.get("key")
        if not isinstance(key, str) or not key or key.lower() == "latest":
            return False
    return True


def inspect(document: dict[str, Any], auth_mode: str) -> dict[str, int]:
    spec = _template_spec(document)
    volumes = spec.get("volumes", [])
    containers = spec.get("containers", [])
    if not isinstance(volumes, list) or not isinstance(containers, list) or not containers:
        raise ValueError("manifest_runtime_shape_invalid")

    volume_names: list[str] = []
    secret_volumes: list[dict[str, Any]] = []
    for volume in volumes:
        if not isinstance(volume, dict):
            raise ValueError("manifest_volume_invalid")
        name = volume.get("name")
        if not isinstance(name, str) or not name:
            raise ValueError("manifest_volume_name_invalid")
        volume_names.append(name)
        secret = volume.get("secret")
        if isinstance(secret, dict):
            secret_volumes.append(volume)

    mount_names: list[str] = []
    mount_paths: list[str] = []
    for container in containers:
        if not isinstance(container, dict):
            raise ValueError("manifest_container_invalid")
        mounts = container.get("volumeMounts", [])
        if not isinstance(mounts, list):
            raise ValueError("manifest_mounts_invalid")
        for mount in mounts:
            if not isinstance(mount, dict):
                raise ValueError("manifest_mount_invalid")
            name = mount.get("name")
            path = mount.get("mountPath")
            if not isinstance(name, str) or not name:
                raise ValueError("manifest_mount_name_invalid")
            if not isinstance(path, str) or not path:
                raise ValueError("manifest_mount_path_invalid")
            mount_names.append(name)
            mount_paths.append(path)

    volume_name_set = set(volume_names)
    mount_name_set = set(mount_names)
    secret_name_set = {v["name"] for v in secret_volumes}

    mounted_secret_names = [name for name in mount_names if name in secret_name_set]
    duplicate_volume_names = len(volume_names) - len(volume_name_set)
    duplicate_mount_names = len(mount_names) - len(mount_name_set)
    duplicate_mount_paths = len(mount_paths) - len(set(mount_paths))
    missing_volume_bindings = sum(1 for name in mount_names if name not in volume_name_set)
    orphan_secret_volumes = sum(1 for v in secret_volumes if v["name"] not in mount_name_set)
    unpinned_secret_volumes = sum(
        1 for v in secret_volumes if not _version_is_pinned(v["secret"])
    )
    duplicate_secret_sources = len(secret_volumes) - len(
        {
            (
                v["secret"].get("secretName"),
                tuple(
                    (item.get("key"), item.get("path"))
                    for item in (v["secret"].get("items") or [])
                    if isinstance(item, dict)
                ),
            )
            for v in secret_volumes
        }
    )

    return {
        "expected_secret_mounts": EXPECTED_SECRET_MOUNTS[auth_mode],
        "secret_volumes": len(secret_volumes),
        "mounted_secret_volumes": len(mounted_secret_names),
        "duplicate_volume_names": duplicate_volume_names,
        "duplicate_mount_names": duplicate_mount_names,
        "duplicate_mount_paths": duplicate_mount_paths,
        "missing_volume_bindings": missing_volume_bindings,
        "orphan_secret_volumes": orphan_secret_volumes,
        "unpinned_secret_volumes": unpinned_secret_volumes,
        "duplicate_secret_sources": duplicate_secret_sources,
    }


def failures(result: dict[str, int]) -> list[tuple[str, int]]:
    checks = [
        ("duplicate_volume_name", result["duplicate_volume_names"]),
        ("duplicate_mount_name", result["duplicate_mount_names"]),
        ("duplicate_mount_path", result["duplicate_mount_paths"]),
        ("missing_volume_binding", result["missing_volume_bindings"]),
        ("orphan_secret_volume", result["orphan_secret_volumes"]),
        ("unpinned_secret_version", result["unpinned_secret_volumes"]),
        ("duplicate_secret_source", result["duplicate_secret_sources"]),
    ]
    expected = result["expected_secret_mounts"]
    actual = result["mounted_secret_volumes"]
    if actual != expected:
        checks.append(("secret_mount_count_mismatch", abs(actual - expected)))
    return [(code, count) for code, count in checks if count]


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate a Cloud Run secret-volume graph using bounded, content-free output."
    )
    parser.add_argument("manifest", type=Path)
    parser.add_argument(
        "--auth-mode",
        choices=sorted(EXPECTED_SECRET_MOUNTS),
        required=True,
    )
    args = parser.parse_args()

    try:
        document = json.loads(args.manifest.read_text(encoding="utf-8"))
        if not isinstance(document, dict):
            raise ValueError("manifest_root_invalid")
        result = inspect(document, args.auth_mode)
    except (OSError, json.JSONDecodeError, ValueError):
        print("FAIL code=manifest_invalid count=1")
        return 2

    found = failures(result)
    if found:
        for code, count in found:
            print(f"FAIL code={code} count={count}")
        return 1

    print(
        "PASS "
        f"secret_mounts={result['mounted_secret_volumes']} "
        f"secret_volumes={result['secret_volumes']} "
        "pinned_versions=yes"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
