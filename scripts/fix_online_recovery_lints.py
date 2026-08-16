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
replace_once(
    "src/bin/v2_recover.rs",
    "use std::path::PathBuf;\n",
    "use std::path::{Path, PathBuf};\n",
)
replace_once(
    "src/bin/v2_recover.rs",
    "    state_dir: &PathBuf,\n    hub_public_key_file: &PathBuf,\n",
    "    state_dir: &Path,\n    hub_public_key_file: &Path,\n",
)

print("online recovery lint fixes applied")
