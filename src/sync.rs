use std::{path::Path, process::Stdio, sync::Arc};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::{
    process::Command,
    sync::RwLock,
    time::{Duration, sleep},
};
use tracing::{error, info, warn};

use crate::config::AppConfig;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncStatus {
    pub current_sha: Option<String>,
    pub previous_sha: Option<String>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

pub async fn sync_loop(config: AppConfig, status: Arc<RwLock<SyncStatus>>) {
    loop {
        if let Err(err) = sync_once(&config, status.clone()).await {
            error!("sync loop error: {err:#}");
        }
        sleep(Duration::from_secs(config.git_sync_interval_seconds)).await;
    }
}

pub async fn sync_once(config: &AppConfig, status: Arc<RwLock<SyncStatus>>) -> Result<()> {
    {
        let mut write = status.write().await;
        write.last_attempt_at = Some(Utc::now());
    }

    let result = ensure_repo_synced(config).await;
    match result {
        Ok(sha) => {
            let mut write = status.write().await;
            if write.current_sha.as_deref() != Some(sha.as_str()) {
                write.previous_sha = write.current_sha.clone();
            }
            write.current_sha = Some(sha.clone());
            write.last_success_at = Some(Utc::now());
            write.last_error = None;
            info!("sync successful: {}", sha);
            Ok(())
        }
        Err(err) => {
            let mut write = status.write().await;
            write.last_error = Some(err.to_string());
            Err(err)
        }
    }
}

async fn ensure_repo_synced(config: &AppConfig) -> Result<String> {
    let repo_url = config.repo_url_with_auth();
    let mirror_dir = &config.mirror_dir;
    let branch = &config.git_branch;

    if !mirror_dir.join(".git").exists() {
        if let Some(parent) = mirror_dir.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed creating parent dir {}", parent.display()))?;
        }

        info!("cloning repository into {}", mirror_dir.display());
        run_cmd(
            Command::new("git")
                .arg("clone")
                .arg("--branch")
                .arg(branch)
                .arg("--single-branch")
                .arg(&repo_url)
                .arg(mirror_dir),
            "git clone",
        )
        .await?;
    } else if !Path::new(mirror_dir).exists() {
        return Err(anyhow!(
            "mirror dir does not exist: {}",
            mirror_dir.display()
        ));
    }

    run_git_in(
        config,
        ["remote", "set-url", "origin", &repo_url],
        "git remote set-url",
    )
    .await?;
    run_git_in(
        config,
        ["fetch", "origin", branch, "--prune"],
        "git fetch origin branch",
    )
    .await?;
    run_git_in(
        config,
        ["reset", "--hard", &format!("origin/{branch}")],
        "git reset --hard",
    )
    .await?;
    run_git_in(config, ["clean", "-fd"], "git clean -fd").await?;

    let sha = run_git_in(config, ["rev-parse", "HEAD"], "git rev-parse HEAD")
        .await?
        .trim()
        .to_string();
    if sha.is_empty() {
        return Err(anyhow!("empty commit sha after sync"));
    }

    Ok(sha)
}

async fn run_git_in<const N: usize>(
    config: &AppConfig,
    args: [&str; N],
    label: &str,
) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&config.mirror_dir);
    for arg in args {
        cmd.arg(arg);
    }
    run_cmd(&mut cmd, label).await
}

async fn run_cmd(cmd: &mut Command, label: &str) -> Result<String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = cmd
        .output()
        .await
        .with_context(|| format!("failed running {label}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(anyhow!("{label} failed: {}", stderr.trim()));
    }
    if !stderr.trim().is_empty() {
        warn!("{label} stderr: {}", stderr.trim());
    }

    Ok(stdout)
}
