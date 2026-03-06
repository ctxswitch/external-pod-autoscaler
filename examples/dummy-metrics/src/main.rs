use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
const VALUE_FILE: &str = "/etc/dummy-metrics/value";

#[derive(Clone)]
struct AppState {
    name: String,
    service_dns: String,
}

async fn replica_count(service_dns: &str) -> u64 {
    let lookup = format!("{service_dns}:0");
    let count = tokio::net::lookup_host(&lookup)
        .await
        .map(|addrs| addrs.count() as u64)
        .unwrap_or(0);
    if count == 0 {
        1
    } else {
        count
    }
}

async fn metrics(State(state): State<AppState>) -> Response {
    let raw: f64 = tokio::task::spawn_blocking(|| {
        std::fs::read_to_string(VALUE_FILE)
            .unwrap_or_else(|_| "0".into())
            .trim()
            .parse::<f64>()
            .unwrap_or(0.0)
    })
    .await
    .unwrap_or(0.0);

    let replicas = replica_count(&state.service_dns).await;
    let value = raw / replicas as f64;

    let name = &state.name;
    let body = format!("# TYPE {name} gauge\n{name} {value}\n");
    (
        StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("Usage: dummy-metrics <port> <metric_name> <service_dns>");
        std::process::exit(1);
    }

    let port = &args[1];
    let state = AppState {
        name: args[2].clone(),
        service_dns: args[3].clone(),
    };

    let app = Router::new()
        .route("/metrics", get(metrics))
        .with_state(state.clone());

    let addr = format!("0.0.0.0:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    eprintln!(
        "dummy-metrics serving {} on {addr} (dns: {})",
        state.name, state.service_dns
    );
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}
