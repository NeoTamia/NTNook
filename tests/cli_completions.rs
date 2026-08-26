use std::fs;
use std::process::{Command, Output};

use uuid::Uuid;

#[test]
fn completion_scripts_are_generated_without_configuration_or_caddy() {
    for (shell, markers) in [
        (
            "bash",
            &[
                "_nook()",
                "complete -F _nook",
                "--readiness-warn-after",
                "update",
            ][..],
        ),
        (
            "zsh",
            &[
                "#compdef nook",
                "compdef _nook nook",
                "--readiness-warn-after",
                "update",
            ][..],
        ),
    ] {
        let output = nook(&[
            "--caddy-socket",
            "/definitely/missing",
            "completions",
            shell,
        ]);
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());

        let script = String::from_utf8(output.stdout).unwrap();
        for marker in markers {
            assert!(
                script.contains(marker),
                "{shell} completion omitted {marker}"
            );
        }
    }
}

#[test]
fn completion_help_lists_supported_shells_and_rejects_others() {
    let help = nook(&["completions", "--help"]);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("Usage: nook completions <SHELL>"));
    assert!(help.contains("[possible values: bash, zsh]"));

    let rejected = nook(&["completions", "fish"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("invalid value 'fish'"));
}

#[test]
fn generated_bash_completion_has_valid_syntax() {
    let output = nook(&["completions", "bash"]);
    assert!(output.status.success());

    let directory = std::env::temp_dir().join(format!("nook-cli-completions-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("nook.bash");
    fs::write(&path, output.stdout).unwrap();

    let syntax = Command::new("bash")
        .args(["-n"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        syntax.status.success(),
        "{}",
        String::from_utf8_lossy(&syntax.stderr)
    );
    fs::remove_dir_all(directory).unwrap();
}

fn nook(arguments: &[&str]) -> Output {
    let directory = std::env::temp_dir().join(format!("nook-completions-env-{}", Uuid::new_v4()));
    let config_home = directory.join("config");
    let config_directory = config_home.join("nook");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(config_directory.join("config.toml"), "invalid = [").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", directory.join("state"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(arguments)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}
