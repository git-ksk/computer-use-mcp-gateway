#!/usr/bin/env python3
"""Fail when repository Markdown points at a missing local file.

External URLs and same-document anchors are intentionally not fetched here.
The goal is a deterministic CI guard for repository documentation paths.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
DOC_GLOBS = ("README.md", "CONTRIBUTING.md", "docs/**/*.md")
LINK_RE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")


def markdown_files() -> list[Path]:
    paths: set[Path] = set()
    for pattern in DOC_GLOBS:
        paths.update(ROOT.glob(pattern))
    return sorted(path for path in paths if path.is_file())


def local_target(source: Path, raw: str) -> Path | None:
    target = raw.strip()
    if not target or target.startswith("#"):
        return None

    # Markdown destinations may be wrapped in angle brackets.
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1]

    parsed = urlsplit(target)
    if parsed.scheme or parsed.netloc:
        return None

    path_text = unquote(parsed.path)
    if not path_text:
        return None

    if path_text.startswith("/"):
        candidate = ROOT / path_text.lstrip("/")
    else:
        candidate = source.parent / path_text
    return candidate.resolve()


def main() -> int:
    failures: list[str] = []
    root = ROOT.resolve()

    for source in markdown_files():
        text = source.read_text(encoding="utf-8")
        for match in LINK_RE.finditer(text):
            raw = match.group(1)
            target = local_target(source, raw)
            if target is None:
                continue
            try:
                target.relative_to(root)
            except ValueError:
                failures.append(f"{source.relative_to(ROOT)}: local link escapes repository: {raw}")
                continue
            if not target.exists():
                failures.append(f"{source.relative_to(ROOT)}: missing local link target: {raw}")

    if failures:
        print("documentation link check failed:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1

    print(f"documentation link check passed ({len(markdown_files())} Markdown files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
