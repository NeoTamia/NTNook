use serde_json::{Value, json};
use std::path::Path;
use uuid::Uuid;

pub fn registry(root: &Path, owner: Uuid, active: bool) -> Value {
    json!({
        "format_version":3,"last_synchronized_at_unix_ms":null,
        "compose_projects":{"identity":{
            "id":Uuid::new_v4(),"identity":"identity","root":root.canonicalize().unwrap(),
            "name":"app","namespace":"app","host":"unix:///unused.sock","engine":"engine",
            "services":["web"],"routes":{"web/http":{
                "service":"web","port":8080,"active":active,"desired":true,"status":"stopped",
                "alias":{"id":owner,"hostname":"web.localhost","target":"http://127.0.0.1:8080/",
                    "scheme":"http","tls":false,"preserve_host":true}
            }}
        }}
    })
}
