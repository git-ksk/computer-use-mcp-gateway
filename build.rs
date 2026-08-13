fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/v2_agent.proto");
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
