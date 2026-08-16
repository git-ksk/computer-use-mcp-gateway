from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one match, got {count}: {old!r}")
    p.write_text(text.replace(old, new, 1))

replace_once(
    "src/v2_online_recovery.rs",
    "    let mut file = match File::open(path) {\n",
    "    let file = match File::open(path) {\n",
)
replace_once(
    "src/bin/v2_recover.rs",
    "use anyhow::{Context, Result, bail};\n",
    "use anyhow::{Context, Result};\n#[cfg(not(target_os = \"macos\"))]\nuse anyhow::bail;\n",
)

print("online recovery lint fixes applied")
