use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router as AxumRouter};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use srvcs_floatadd::{api::Deps, health, router, telemetry};
use tower::ServiceExt;

/// Spin up a *computing* mock `srvcs-isnumber`: it answers `POST /` with
/// `{"result": <bool>}` where the bool is true iff the posted `value` is a JSON
/// number. This mirrors the real leaf and lets us test orchestration without the
/// rest of the fleet.
async fn spawn_isnumber() -> String {
    let app = AxumRouter::new().route(
        "/",
        post(|Json(body): Json<Value>| async move {
            let is_number = body.get("value").map(Value::is_number).unwrap_or(false);
            (StatusCode::OK, Json(json!({ "result": is_number })))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn app(isnumber_url: &str) -> axum::Router {
    router(
        telemetry::metrics_handle_for_tests(),
        Deps {
            isnumber_url: isnumber_url.to_string(),
        },
    )
}

async fn eval(isnumber_url: &str, a: Value, b: Value) -> (StatusCode, Value) {
    let res = app(isnumber_url)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "a": a, "b": b }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Approximate float comparison — never use exact equality on `f64`.
fn approx(got: f64, expected: f64) -> bool {
    (got - expected).abs() < 1e-9
}

// A base URL with nothing listening — exercises the degraded path.
const DEAD_URL: &str = "http://127.0.0.1:1";

async fn status_of(uri: &str) -> StatusCode {
    app(DEAD_URL)
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn index_ok() {
    assert_eq!(status_of("/").await, StatusCode::OK);
}

#[tokio::test]
async fn healthz_ok() {
    assert_eq!(status_of("/healthz").await, StatusCode::OK);
}

#[tokio::test]
async fn readyz_reflects_state() {
    health::set_ready(true);
    assert_eq!(status_of("/readyz").await, StatusCode::OK);
}

#[tokio::test]
async fn metrics_ok() {
    assert_eq!(status_of("/metrics").await, StatusCode::OK);
}

#[tokio::test]
async fn openapi_ok() {
    assert_eq!(status_of("/openapi.json").await, StatusCode::OK);
}

#[tokio::test]
async fn generates_request_id_when_absent() {
    let res = app(DEAD_URL)
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        res.headers().contains_key("x-request-id"),
        "response must carry a generated x-request-id"
    );
}

#[tokio::test]
async fn index_reports_identity() {
    let (status, body) = (status_of("/").await, eval_index().await);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["service"], "srvcs-floatadd");
    assert_eq!(body["depends_on"], json!(["srvcs-isnumber"]));
}

async fn eval_index() -> Value {
    let res = app(DEAD_URL)
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn adds_integers() {
    let isnumber = spawn_isnumber().await;
    let (status, body) = eval(&isnumber, json!(2), json!(3)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(approx(body["result"].as_f64().unwrap(), 5.0));
}

#[tokio::test]
async fn adds_floats() {
    let isnumber = spawn_isnumber().await;
    let (status, body) = eval(&isnumber, json!(1.5), json!(2.25)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(approx(body["result"].as_f64().unwrap(), 3.75));
}

#[tokio::test]
async fn adds_mixed_int_and_float() {
    let isnumber = spawn_isnumber().await;
    let (status, body) = eval(&isnumber, json!(10), json!(0.125)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(approx(body["result"].as_f64().unwrap(), 10.125));
}

#[tokio::test]
async fn adds_imprecise_decimals_approximately() {
    let isnumber = spawn_isnumber().await;
    let (status, body) = eval(&isnumber, json!(0.1), json!(0.2)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(approx(body["result"].as_f64().unwrap(), 0.3));
}

#[tokio::test]
async fn adds_negatives() {
    let isnumber = spawn_isnumber().await;
    let (status, body) = eval(&isnumber, json!(-7.5), json!(-8.25)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(approx(body["result"].as_f64().unwrap(), -15.75));
}

#[tokio::test]
async fn echoes_operands() {
    let isnumber = spawn_isnumber().await;
    let (_, body) = eval(&isnumber, json!(1.5), json!(2.25)).await;
    assert_eq!(body["a"], json!(1.5));
    assert_eq!(body["b"], json!(2.25));
}

#[tokio::test]
async fn rejects_non_number_a() {
    let isnumber = spawn_isnumber().await;
    let (status, _) = eval(&isnumber, json!("nope"), json!(3)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn rejects_non_number_b() {
    let isnumber = spawn_isnumber().await;
    let (status, _) = eval(&isnumber, json!(3), json!(true)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn degrades_when_isnumber_is_unreachable() {
    let (status, body) = eval(DEAD_URL, json!(2.0), json!(3.0)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["dependency"], "srvcs-isnumber");
}
