use axum::{body::Body, http::Request};
use sqlx::PgPool;
use tower::ServiceExt;
mod common;
#[sqlx::test]
async fn published_contract_and_ui_require_current_membership(pool: PgPool) {
    common::seed_org_and_clinician(&pool, common::ORG_A, "docs-doc").await;
    let app = er_server::routes::router(common::test_state(pool).await);
    for path in ["/api-docs", "/api-docs/openapi.json"] {
        assert_eq!(common::get(&app, path, None).await, 401);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(
                        "authorization",
                        format!("Bearer {}", common::mint("docs-doc")),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../../../docs/api/openapi.json")).unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(
        doc["paths"]["/v1/patients/{id}/mdm"]["post"]["responses"]["202"]["description"],
        "Success"
    );
    assert!(doc["components"]["schemas"]["NoteEvent"]["oneOf"].is_array());
}
