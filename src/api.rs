use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::{OpenApi, ToSchema};

use crate::client::{self, DepError};

pub const SERVICE: &str = "srvcs-floatadd";
pub const CONCERN: &str = "float arithmetic: a + b";
pub const DEPENDS_ON: &[&str] = &["srvcs-isnumber"];

/// Dependency endpoints, injected as router state so tests can point them at
/// mock services.
#[derive(Clone)]
pub struct Deps {
    pub isnumber_url: String,
}

#[derive(Serialize, ToSchema)]
pub struct Info {
    pub service: &'static str,
    pub concern: &'static str,
    pub depends_on: Vec<&'static str>,
}

/// `GET /` — service identity (srvcs service standard).
#[utoipa::path(get, path = "/", responses((status = 200, body = Info)))]
pub async fn index() -> Json<Info> {
    Json(Info {
        service: SERVICE,
        concern: CONCERN,
        depends_on: DEPENDS_ON.to_vec(),
    })
}

#[derive(Deserialize, ToSchema)]
pub struct EvalRequest {
    #[schema(value_type = Object)]
    pub a: Value,
    #[schema(value_type = Object)]
    pub b: Value,
}

#[derive(Serialize, ToSchema)]
pub struct SumResponse {
    #[schema(value_type = Object)]
    pub a: Value,
    #[schema(value_type = Object)]
    pub b: Value,
    pub result: f64,
}

/// The single concern: the floating-point sum of two reals. No domain
/// restriction — any two finite or non-finite `f64` values may be added.
pub fn float_add(a: f64, b: f64) -> f64 {
    a + b
}

fn ok(a: Value, b: Value, result: f64) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "a": a, "b": b, "result": result })),
    )
        .into_response()
}

fn invalid(reason: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "error": reason })),
    )
        .into_response()
}

fn degraded(dependency: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "dependency unavailable", "dependency": dependency })),
    )
        .into_response()
}

/// Validate a single operand via `srvcs-isnumber`, then coerce it to an `f64`.
///
/// Accepts both integers and floats (this is a float service). Returns
/// `Ok(f64)` on success, or an error `Response` (422/503) the caller should
/// return verbatim.
async fn ask(isnumber_url: &str, value: &Value) -> Result<f64, Response> {
    match client::call(isnumber_url, &json!({ "value": value })).await {
        Err(DepError::Unreachable) => Err(degraded("srvcs-isnumber")),
        Ok((200, body)) => {
            let is_number = body.get("result").and_then(Value::as_bool).unwrap_or(false);
            if !is_number {
                return Err(invalid("value is not a number"));
            }
            value
                .as_f64()
                .ok_or_else(|| invalid("value is not a number"))
        }
        // Invalid input propagates from the leaf dependency; forward its 422.
        Ok((422, body)) => Err((StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response()),
        Ok(_) => Err(degraded("srvcs-isnumber")),
    }
}

/// `POST /` — compute `a + b` as an `f64`.
///
/// Input validation is delegated to `srvcs-isnumber` over HTTP (the single
/// source of truth for "is this a number"), once per operand. If that
/// dependency is unreachable, this service reports itself degraded rather than
/// guessing.
#[utoipa::path(
    post,
    path = "/",
    request_body = EvalRequest,
    responses(
        (status = 200, body = SumResponse),
        (status = 422, description = "an operand is not a number"),
        (status = 503, description = "a dependency is unavailable")
    )
)]
pub async fn evaluate(State(deps): State<Deps>, Json(req): Json<EvalRequest>) -> Response {
    let a = match ask(&deps.isnumber_url, &req.a).await {
        Ok(n) => n,
        Err(resp) => return resp,
    };
    let b = match ask(&deps.isnumber_url, &req.b).await {
        Ok(n) => n,
        Err(resp) => return resp,
    };

    ok(req.a, req.b, float_add(a, b))
}

#[derive(OpenApi)]
#[openapi(
    paths(index, evaluate),
    components(schemas(Info, EvalRequest, SumResponse))
)]
pub struct ApiDoc;

/// Serve OpenAPI document
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(got: f64, expected: f64) -> bool {
        (got - expected).abs() < 1e-9
    }

    #[test]
    fn openapi_documents_routes() {
        let doc = ApiDoc::openapi();
        let root = doc.paths.paths.get("/").expect("path / present");
        assert!(root.get.is_some());
        assert!(root.post.is_some());
    }

    #[test]
    fn sum_is_correct() {
        assert!(approx(float_add(2.0, 3.0), 5.0));
        assert!(approx(float_add(-4.0, 4.0), 0.0));
        assert!(approx(float_add(0.0, 0.0), 0.0));
        assert!(approx(float_add(-7.5, -8.25), -15.75));
    }

    #[test]
    fn sum_handles_fractions() {
        assert!(approx(float_add(0.1, 0.2), 0.3));
        assert!(approx(float_add(1.5, 2.25), 3.75));
        assert!(approx(float_add(10.125, -0.125), 10.0));
    }

    #[test]
    fn sum_accepts_large_and_small() {
        assert!(approx(float_add(1e10, 2e10), 3e10));
        assert!(approx(float_add(1.0e-9, 1.0e-9), 2.0e-9));
    }
}
