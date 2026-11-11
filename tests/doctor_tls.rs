#![cfg(unix)]
mod support;

use serde_json::{Value, json};
use std::process::Command;
use support::caddy::CaddyHarness;

#[test]
fn active_probe_preserves_sni_and_verifies_supplied_ca_without_installation() {
    let caddy = CaddyHarness::start();
    // Explicit fixture setup, never part of the diagnostic path.
    let ca: Value = ureq::get(format!("{}/pki/ca/local", caddy.admin_url()))
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let path = caddy.root().join("diagnostic-ca.pem");
    std::fs::write(&path, ca["root_certificate"].as_str().unwrap()).unwrap();
    for (certificate, expected) in [(Value::Null, false), (json!(path), true)] {
        let output = Command::new(env!("CARGO_BIN_EXE_nook")).arg("__doctor_probe").arg(json!({"Http":{"url":caddy.https_url(),"address":"127.0.0.1","ca_cert":certificate}}).to_string()).output().unwrap();
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.get("Ok").is_some(), expected, "{result}");
        if expected {
            assert_eq!(result["Ok"]["status"], 200);
        }
    }
}
