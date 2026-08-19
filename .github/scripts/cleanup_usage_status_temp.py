from pathlib import Path

for path in ["docs/v2/STATUS.md", "docs/v2/STATUS.ja.md"]:
    p = Path(path)
    lines = p.read_text().splitlines(keepends=True)
    lines = [line for line in lines if "V2_USAGE_ACCOUNTING.md" not in line]
    p.write_text("".join(lines))
