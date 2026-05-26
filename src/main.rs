use axum::{
    http::StatusCode,
    routing::get,
    Router,
};

#[tokio::main]
async fn main() {
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "10000".to_string())
        .parse()
        .expect("PORT must be a number");

    let app = Router::new().route("/health", get(health));
    let bind_addr = format!("0.0.0.0:{port}");
    println!("Starting cerebrum on {bind_addr}");

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("Failed to bind TCP listener");

    axum::serve(listener, app)
        .await
        .expect("Axum server exited unexpectedly");
}

async fn health() -> (StatusCode, &'static str) {
    println!("Health check request received");
    (StatusCode::OK, "ok")
}
