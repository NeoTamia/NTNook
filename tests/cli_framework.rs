use std::process::Command;

#[test]
fn framework_and_no_framework_cannot_be_combined() {
    for arguments in [
        ["run", "--framework", "vite", "--no-framework", "--", "vite"].as_slice(),
        [
            "run",
            "--no-framework",
            "--framework",
            "nuxt",
            "--",
            "nuxt",
            "dev",
        ]
        .as_slice(),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_nook"))
            .args(arguments)
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "combined flags should fail: {arguments:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cannot be used with"),
            "expected a flag conflict, got {stderr}"
        );
    }
}
