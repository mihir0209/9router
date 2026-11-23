pub mod auth;
pub mod cache;
pub mod circuit_breaker;
pub mod cli;
pub mod combo_router;
pub mod config;
pub mod custom_adapter;
pub mod db;
pub mod embed;
pub mod gemini;
pub mod media;
pub mod mitm;
pub mod original;
pub mod proxy;
pub mod proxy_pools;
pub mod routes;
pub mod sse;
pub mod token_refresh;
pub mod token_saver;
pub mod translator;
pub mod tunnel;

use std::net::SocketAddr;
use std::sync::Arc;

fn report_fallbacks() {
    if let Some(path) = std::env::var_os("RUST_ANYTHING_FALLBACK_LOG") {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path);
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut stream) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            stream.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    report_fallbacks();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if cli::dispatch(&args).await? {
        return Ok(());
    }

    // 1. Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ninerouter=info,tower_http=info".into()),
        )
        .init();

    // 2. Initialize configuration
    let config = Arc::new(config::Config::load());
    tracing::info!(
        "Loaded configuration: data_dir={}, port={}",
        config.data_dir.display(),
        config.port
    );

    // 3. Initialize SQLite database & write-behind worker task
    let db = db::Database::open(&config)?;
    tracing::info!("Database initialized at {}", config.db_path.display());

    // 4. Start background token lifecycle & model catalog refresh worker
    let token_refresh_handle = token_refresh::start_background_token_refresh(
        config.token_refresh.clone(),
        Arc::clone(&db),
        &config.data_dir,
    );

    // 5. Initialize ProxyService
    let proxy = Arc::new(proxy::ProxyService::new());
    if let Ok(count) = proxy.sync_active_proxies_from_db(&db) {
        if count > 0 {
            tracing::info!("Loaded {} active proxy pools into outbound rotator", count);
        }
    }

    // 6. Initialize Original Next.js dashboard backend
    let original = original::Original::start().await;
    tracing::info!("Original Next.js dashboard backend initialized at {}", original.base);

    // 7. Compose Axum router
    let circuit_breaker = Arc::new(circuit_breaker::CircuitBreakerRegistry::default());
    let combo_tracker = combo_router::ComboTracker::with_circuit_breakers(Arc::clone(&circuit_breaker));
    let token_cache = cache::TokenCache::default();
    let state = routes::AppState {
        config: Arc::clone(&config),
        db,
        proxy,
        original: original.clone(),
        combo_tracker,
        token_cache,
        circuit_breaker,
        console_buffer: routes::ConsoleLogBuffer::new(),
    };
    let app = routes::create_router(state);

    // 8. Bind to port and run with graceful shutdown
    let port = config.port;
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let addr = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => SocketAddr::new(ip, port),
        Err(_) => SocketAddr::from(([0, 0, 0, 0], port)),
    };
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!(
                "\x1b[31mError:\x1b[0m Address {} is already in use.\nAnother 9Router instance or service is already listening on this port.\nUse '9r status' to inspect the running daemon, or specify a different port with 'PORT=<port> 9r'.",
                addr
            );
            std::process::exit(1);
        }
        Err(e) => return Err(e.into()),
    };
    tracing::info!("9Router listening on http://{}:{}", host, port);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    if let Some(h) = token_refresh_handle {
        h.stop();
    }
    original.stop();
    tracing::info!("9Router shutdown gracefully");
    Ok(())
}
