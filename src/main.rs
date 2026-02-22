use std::sync::Arc;

use anyhow::Result;
use repo_sync::{
    config::AppConfig,
    server::{AppState, router},
    sync::{SyncStatus, sync_loop, sync_once},
};
use tokio::{net::TcpListener, sync::RwLock};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("repo_sync=info,tower_http=info,axum=info")),
        )
        .init();

    let config = AppConfig::from_env()?;
    let status = Arc::new(RwLock::new(SyncStatus::default()));

    sync_once(&config, status.clone()).await?;
    let sync_config = config.clone();
    let sync_status = status.clone();
    tokio::spawn(async move {
        sync_loop(sync_config, sync_status).await;
    });

    let state = AppState { config, status };
    let app = router(state.clone());
    let listener = TcpListener::bind(&state.config.http_bind_addr).await?;
    info!("listening on {}", state.config.http_bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}
