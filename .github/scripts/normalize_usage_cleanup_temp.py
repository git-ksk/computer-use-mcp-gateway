from pathlib import Path

for path in [".env.example", "packaging/README.md"]:
    p = Path(path)
    p.write_text(p.read_text().rstrip() + "\n")
