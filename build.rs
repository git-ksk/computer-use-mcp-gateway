fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/v2_agent.proto");
    println!("cargo:rerun-if-env-changed=CUMG_SOURCE_COMMIT");

    let package_version = std::env::var("CARGO_PKG_VERSION")?;
    let source_commit = match std::env::var("CUMG_SOURCE_COMMIT") {
        Ok(value) => {
            if value.len() != 40
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(
                    "CUMG_SOURCE_COMMIT must be exactly 40 lowercase hexadecimal characters".into(),
                );
            }
            value
        }
        Err(std::env::VarError::NotPresent) => "unknown".to_owned(),
        Err(error) => return Err(error.into()),
    };
    println!("cargo:rustc-env=CUMG_BUILD_SOURCE_COMMIT={source_commit}");
    println!("cargo:rustc-env=CUMG_BUILD_VERSION={package_version} (commit {source_commit})");
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost = prost_build::Config::new();
    prost.protoc_executable(protoc);
    tonic_prost_build::configure().compile_with_config(
        prost,
        &["proto/v2_agent.proto"],
        &["proto"],
    )?;
    Ok(())
}
