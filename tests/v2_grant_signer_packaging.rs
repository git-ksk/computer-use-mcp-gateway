#[test]
fn production_hub_unit_does_not_receive_the_grant_signing_secret() {
    let hub = include_str!("../packaging/systemd/cumg-v2-hub.service");
    let signer = include_str!("../packaging/systemd/cumg-v2-grant-signer.service");
    let env = include_str!("../packaging/systemd/hub.env.example");

    assert!(!hub.contains("LoadCredentialEncrypted=grant-secret"));
    assert!(!hub.contains("CUMG_V2_GRANT_SECRET_FILE"));
    assert!(hub.contains("CUMG_V2_GRANT_SIGNER_SOCKET"));
    assert!(hub.contains("Requires=cumg-v2-grant-signer.service"));

    assert!(signer.contains("User=cumg-v2-signer"));
    assert!(signer.contains("LoadCredentialEncrypted=grant-secret"));
    assert!(signer.contains("CUMG_V2_GRANT_SECRET_FILE=%d/grant-secret"));
    assert!(signer.contains("RestrictAddressFamilies=AF_UNIX"));

    assert!(env.contains("CUMG_V2_GRANT_PUBLIC_KEY_FILE="));
    assert!(!env.contains("CUMG_V2_GRANT_SECRET_FILE="));
}
