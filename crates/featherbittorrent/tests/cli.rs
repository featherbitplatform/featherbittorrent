#[test]
fn version_prints_name_and_version() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_featherbittorrent"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        format!("featherbittorrent {}", env!("CARGO_PKG_VERSION"))
    );
}
