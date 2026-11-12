// SPDX-License-Identifier: GPL-3.0-or-later
//! `/v1/mesh` handlers (spec section 5). Same listener and bearer auth as
//! the rest of the API. Handlers never touch the kernel directly: they
//! update desired state and the manager reconciles.

// axum handlers return the Response they want sent; boxing it buys nothing here.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;

use crate::mesh::{FieldError, MeshManager, MeshSpec, MeshStatus, PeerStatus, PutError};

#[derive(Clone)]
pub struct MeshApi {
    pub mesh: Arc<MeshManager>,
}

#[derive(Serialize)]
pub struct MeshDoc {
    pub spec: MeshSpec,
    pub status: MeshStatus,
}

#[derive(Serialize)]
struct ValidationBody {
    errors: Vec<FieldError>,
}

#[derive(Serialize)]
struct DeleteBody {
    status: MeshStatus,
    /// True when `[mesh]` in xdebbied.toml has `enabled = true`, so the
    /// mesh was rebuilt from config right after teardown.
    reenabled_from_config: bool,
}

#[derive(Serialize)]
struct ErrBody {
    error: String,
}

pub fn router(api: MeshApi) -> Router {
    Router::new()
        .route("/mesh", get(get_mesh).put(put_mesh).delete(delete_mesh))
        .route("/mesh/peers/{box}", get(get_peer))
        .route("/mesh/reconcile", post(reconcile))
        .with_state(api)
}

async fn get_mesh(State(a): State<MeshApi>) -> Json<MeshDoc> {
    let (spec, status) = a.mesh.get().await;
    Json(MeshDoc { spec, status })
}

/// Optional `If-Match: <generation>`; mismatch → 409.
fn if_match(h: &HeaderMap) -> Result<Option<u64>, Response> {
    match h.get("if-match") {
        None => Ok(None),
        Some(v) => {
            let s = v.to_str().unwrap_or("").trim().trim_matches('"');
            s.parse::<u64>().map(Some).map_err(|_| {
                (StatusCode::BAD_REQUEST, Json(ErrBody { error: format!("If-Match must be a generation number, got {s:?}") }))
                    .into_response()
            })
        }
    }
}

async fn put_mesh(State(a): State<MeshApi>, headers: HeaderMap, Json(spec): Json<MeshSpec>) -> Response {
    let im = match if_match(&headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match a.mesh.put(spec, im).await {
        Ok((spec, status)) => (StatusCode::OK, Json(MeshDoc { spec, status })).into_response(),
        Err(PutError::Validation(errors)) => (StatusCode::BAD_REQUEST, Json(ValidationBody { errors })).into_response(),
        Err(PutError::Conflict { current }) => (
            StatusCode::CONFLICT,
            Json(ErrBody { error: format!("If-Match does not match current generation {current}") }),
        )
            .into_response(),
        Err(PutError::Internal(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrBody { error: format!("{e:#}") })).into_response(),
    }
}

async fn delete_mesh(State(a): State<MeshApi>) -> Response {
    match a.mesh.delete().await {
        Ok((status, reenabled_from_config)) => (StatusCode::OK, Json(DeleteBody { status, reenabled_from_config })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrBody { error: format!("{e:#}") })).into_response(),
    }
}

async fn get_peer(State(a): State<MeshApi>, Path(b): Path<u8>) -> Result<Json<PeerStatus>, Response> {
    a.mesh
        .peer(b)
        .await
        .map(Json)
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(ErrBody { error: format!("no peer with box {b}") })).into_response())
}

async fn reconcile(State(a): State<MeshApi>) -> StatusCode {
    a.mesh.trigger();
    StatusCode::ACCEPTED
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use axum::{
        body::Body,
        http::{Request, header},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::mesh::{FakeNetOps, MeshConfig, PeerSpec};

    fn setup() -> (Router, Arc<FakeNetOps>, tempfile::TempDir) {
        let f = Arc::new(FakeNetOps::new());
        f.add_bridge("br-int", 1500, "52:54:00:23:00:fe");
        f.add_underlay("ul0", 1420, Ipv4Addr::new(10, 22, 0, 1));
        let dir = tempfile::tempdir().unwrap();
        let cfg = MeshConfig { underlay: "ul0".into(), allow_plain_underlay: true, ..Default::default() };
        let mesh = MeshManager::new_for_test(f.clone(), dir.path().into(), Some(&cfg)).unwrap();
        let app = Router::new().nest("/v1", router(MeshApi { mesh }));
        (app, f, dir)
    }

    fn spec(n: u8) -> MeshSpec {
        MeshSpec {
            enabled: true,
            underlay: "ul0".into(),
            allow_plain_underlay: true,
            peers: (1..=n).map(|i| PeerSpec::new(i, Ipv4Addr::new(10, 22, i, 1))).collect(),
            ..Default::default()
        }
    }

    async fn call(app: &Router, method: &str, path: &str, body: Option<&MeshSpec>, if_match: Option<&str>) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method(method).uri(path).header(header::CONTENT_TYPE, "application/json");
        if let Some(im) = if_match {
            req = req.header("If-Match", im);
        }
        let req = req.body(Body::from(body.map(|b| serde_json::to_vec(b).unwrap()).unwrap_or_default())).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let st = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v = if bytes.is_empty() { serde_json::Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
        (st, v)
    }

    #[tokio::test]
    async fn get_before_configure() {
        let (app, _, _d) = setup();
        let (st, v) = call(&app, "GET", "/v1/mesh", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["spec"]["enabled"], false);
        assert_eq!(v["status"]["source"], "config");
        assert_eq!(v["status"]["peers"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn put_validation_400() {
        let (app, _, _d) = setup();
        let mut bad = spec(1);
        bad.peers[0].box_id = 0;
        let (st, v) = call(&app, "PUT", "/v1/mesh", Some(&bad), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(v["errors"][0]["field"], "peers[0].box");
    }

    #[tokio::test]
    async fn put_if_match_409_and_idempotent() {
        let (app, f, _d) = setup();
        let (st, v) = call(&app, "PUT", "/v1/mesh", Some(&spec(2)), None).await;
        assert_eq!(st, StatusCode::OK);
        let g = v["status"]["generation"].as_u64().unwrap();
        assert_eq!(v["status"]["observed_generation"].as_u64().unwrap(), g);

        let (st, _) = call(&app, "PUT", "/v1/mesh", Some(&spec(3)), Some("1")).await;
        assert_eq!(st, StatusCode::CONFLICT);

        f.clear_applied();
        let (st, v) = call(&app, "PUT", "/v1/mesh", Some(&spec(2)), Some(&g.to_string())).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["status"]["generation"].as_u64().unwrap(), g, "idempotent PUT keeps generation");
        assert!(f.applied().is_empty());

        let (st, v) = call(&app, "GET", "/v1/mesh/peers/2", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["device"], "gnv-b2");
        let (st, _) = call(&app, "GET", "/v1/mesh/peers/7", None, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_and_reconcile() {
        let (app, f, _d) = setup();
        call(&app, "PUT", "/v1/mesh", Some(&spec(1)), None).await;
        let (st, _) = call(&app, "POST", "/v1/mesh/reconcile", None, None).await;
        assert_eq!(st, StatusCode::ACCEPTED);
        let (st, v) = call(&app, "DELETE", "/v1/mesh", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["reenabled_from_config"], false);
        assert!(!f.snapshot().links.contains_key("gnv-b1"));
        let (st, _) = call(&app, "DELETE", "/v1/mesh", None, None).await;
        assert_eq!(st, StatusCode::OK, "idempotent");
    }
}
