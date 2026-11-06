//! Regenerate the committed OpenAPI 3.1 snapshot from the actual serde types.
use er_api::*;
use schemars::JsonSchema;
use serde_json::{Value, json};
fn add<T: JsonSchema>(schemas: &mut Value, name: &str) {
    let mut root = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    if let Some(defs) = root.as_object_mut().unwrap().remove("$defs") {
        for (key, value) in defs.as_object().unwrap() {
            schemas[key] = value.clone();
        }
    }
    root.as_object_mut().unwrap().remove("$schema");
    schemas[name] = root;
}
fn refs(value: &mut Value) {
    match value {
        Value::String(s) if s.starts_with("#/$defs/") => {
            *s = s.replace("#/$defs/", "#/components/schemas/")
        }
        Value::Object(map) => map.values_mut().for_each(refs),
        Value::Array(values) => values.iter_mut().for_each(refs),
        _ => {}
    }
}
fn main() {
    let mut schemas = json!({});
    macro_rules! types { ($($t:ty),*) => { $(add::<$t>(&mut schemas, stringify!($t));)* }; }
    types!(
        RegisterPatientRequest,
        PatientView,
        ShiftView,
        NoteView,
        StatsView,
        AppendEventsRequest,
        AppendEventsResponse,
        EventsPage,
        SnapshotUp,
        SnapshotDown,
        AudioUploadRequest,
        AudioUploadResponse,
        ChunkView,
        MdmRequest,
        MdmAccepted,
        JobView
    );
    schemas["Me"] = json!({"type":"object","required":["clinician_id","org_id"],"properties":{"clinician_id":{"type":"string","format":"uuid"},"org_id":{"type":"string","format":"uuid"}}});
    schemas["Health"] =
        json!({"type":"object","required":["status"],"properties":{"status":{"type":"string"}}});
    let routes = [
        ("/healthz", "get", "", "Health", 200),
        ("/readyz", "get", "", "Health", 200),
        ("/v1/me", "get", "", "Me", 200),
        (
            "/v1/patients",
            "post",
            "RegisterPatientRequest",
            "PatientView",
            200,
        ),
        ("/v1/patients/{id}/note", "get", "", "NoteView", 200),
        ("/v1/patients/{id}/note/export", "get", "", "text", 200),
        (
            "/v1/patients/{id}/events",
            "post",
            "AppendEventsRequest",
            "AppendEventsResponse",
            200,
        ),
        ("/v1/patients/{id}/events", "get", "", "EventsPage", 200),
        ("/v1/patients/{id}/snapshot", "put", "SnapshotUp", "", 200),
        ("/v1/patients/{id}/snapshot", "get", "", "SnapshotDown", 200),
        (
            "/v1/patients/{id}/mdm",
            "post",
            "MdmRequest",
            "MdmAccepted",
            202,
        ),
        ("/v1/jobs/{job_id}", "get", "", "JobView", 200),
        (
            "/v1/patients/{id}/encounters/{eid}/audio",
            "post",
            "AudioUploadRequest",
            "AudioUploadResponse",
            200,
        ),
        (
            "/v1/audio/{chunk_id}/complete",
            "post",
            "",
            "ChunkView",
            200,
        ),
        ("/v1/audio/{chunk_id}", "get", "", "ChunkView", 200),
        ("/v1/shifts", "post", "", "ShiftView", 200),
        ("/v1/shifts/active", "get", "", "ShiftView", 200),
        ("/v1/shifts/{id}", "get", "", "ShiftView", 200),
        ("/v1/shifts/{id}/end", "post", "", "ShiftView", 200),
        ("/v1/stats", "get", "", "StatsView", 200),
        ("/api-docs/openapi.json", "get", "", "", 200),
        ("/api-docs", "get", "", "html", 200),
    ];
    let mut paths = json!({});
    for (index, (path, method, input, output, status)) in routes.into_iter().enumerate() {
        let mut op = json!({"operationId":format!("operation_{index}"),"responses":{status.to_string():{"description":"Success"}},"parameters":[]});
        if path.starts_with("/v1") || path.starts_with("/api-docs") {
            op["security"] = json!([{"bearerAuth":[]}]);
            for (code, desc) in [
                (408, "Request timeout; empty body"),
                (409, "State conflict"),
                (
                    401,
                    "Missing/invalid token or inactive/non-physician membership; empty body",
                ),
                (404, "Missing or inaccessible resource"),
                (429, "Clinician quota exhausted; Retry-After: 60"),
                (503, "Quota/audit storage unavailable"),
                (500, "Internal failure; static error code only"),
            ] {
                op["responses"][code.to_string()] = json!({"description":desc});
            }
        }
        for segment in path.split('/') {
            if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                op["parameters"].as_array_mut().unwrap().push(json!({"name":name,"in":"path","required":true,"schema":{"type":"string","format":"uuid"}}));
            }
        }
        if path.ends_with("/events") && method == "get" {
            for (name, default, maximum) in [("since", 0, u64::MAX), ("limit", 500, 1000)] {
                op["parameters"].as_array_mut().unwrap().push(json!({"name":name,"in":"query","schema":{"type":"integer","minimum":0,"default":default,"maximum":maximum}}));
            }
        }
        if !input.is_empty() {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{input}")}}}});
            for code in [400, 409, 413, 415, 422] {
                op["responses"][code.to_string()] = json!({"description":"Validation, state, media or body rejection; see docs/api-contract.md"});
            }
        }
        if !output.is_empty() {
            let (mime, schema) = if output == "html" {
                ("text/html", json!({"type":"string"}))
            } else if output == "text" {
                ("text/plain", json!({"type":"string"}))
            } else {
                (
                    "application/json",
                    json!({"$ref":format!("#/components/schemas/{output}")}),
                )
            };
            op["responses"][status.to_string()]["content"] = json!({mime:{"schema":schema}});
        }
        if path == "/readyz" {
            op["responses"]["503"] = json!({"description":"Readiness unavailable","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Health"}}}});
        }
        if path == "/api-docs/openapi.json" {
            op["responses"]["200"]["content"] = json!({"application/json":{"schema":{"type":"object","required":["openapi","info","paths"]}}});
        }
        paths[path][method] = op;
    }
    let mut doc = json!({"openapi":"3.1.0","info":{"title":"ER Note API","version":"1.0.0","description":"Synthetic staging contract. 60 requests/minute per clinician, plus 5 MDM and 30 audio intents/minute. Stable event/job/chunk IDs are required on retries. See docs/api-contract.md."},"servers":[{"url":"/"}],"paths":paths,"components":{"schemas":schemas,"securitySchemes":{"bearerAuth":{"type":"http","scheme":"bearer","bearerFormat":"JWT"}}}});
    refs(&mut doc);
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
