mod support;

use std::fs;
use std::thread;

use support::caddy::CaddyHarness;

#[test]
fn isolated_caddy_exposes_http_https_reload_and_concurrency() {
    let harness = CaddyHarness::start();
    let root = harness.root().to_owned();
    assert_eq!(
        ureq::get(&harness.http_url())
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap(),
        "http-ok"
    );
    let tls_error = ureq::get(&harness.https_url()).call().unwrap_err();
    let tls_error = format!("{tls_error} {tls_error:?}").to_ascii_lowercase();
    assert!(
        tls_error.contains("certificate")
            || tls_error.contains("unknownissuer")
            || tls_error.contains("tls"),
        "expected a TLS trust failure, got {tls_error}"
    );
    harness.reload();

    let url = harness.http_url();
    let requests: Vec<_> = (0..4)
        .map(|_| {
            let url = url.clone();
            thread::spawn(move || ureq::get(&url).call().unwrap().status())
        })
        .collect();
    for request in requests {
        assert_eq!(request.join().unwrap(), 200);
    }
    assert!(
        ureq::get(format!("{}/config/", harness.admin_url()))
            .call()
            .is_ok()
    );
    let root_pem = ureq::get(format!("{}/pki/ca/local", harness.admin_url()))
        .call()
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
    assert!(root_pem.contains("BEGIN CERTIFICATE"));
    drop(harness);
    assert!(!fs::exists(root).unwrap());
}
