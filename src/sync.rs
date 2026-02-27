use std::{fs, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use git2::{
    AutotagOption, Cred, FetchOptions, RemoteCallbacks, Repository, ResetType, Status,
    StatusOptions, build::RepoBuilder,
};
use serde::Serialize;
use tokio::{
    sync::RwLock,
    task,
    time::{Duration, sleep},
};
use tracing::{error, info};

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
    let config = config.clone();
    task::spawn_blocking(move || ensure_repo_synced_blocking(&config))
        .await
        .context("sync task join error")?
}

fn ensure_repo_synced_blocking(config: &AppConfig) -> Result<String> {
    let repo_url = config.git_repo_url.as_str();
    let mirror_dir = &config.mirror_dir;
    let branch = config.git_branch.as_str();

    if !mirror_dir.join(".git").exists() {
        if let Some(parent) = mirror_dir.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed creating parent dir {}", parent.display()))?;
        }
        info!("cloning repository into {}", mirror_dir.display());
        clone_repository(repo_url, mirror_dir, branch, config.git_token.as_deref())?;
    } else if !Path::new(mirror_dir).exists() {
        return Err(anyhow!(
            "mirror dir does not exist: {}",
            mirror_dir.display()
        ));
    }

    let repo = Repository::open(mirror_dir)
        .with_context(|| format!("failed opening repo in {}", mirror_dir.display()))?;
    set_origin_url(&repo, repo_url)?;
    fetch_branch(&repo, branch, config.git_token.as_deref())?;
    hard_reset_to_origin_branch(&repo, branch)?;
    clean_untracked(&repo)?;

    let head = repo.head().context("failed reading HEAD")?;
    let oid = head
        .target()
        .ok_or_else(|| anyhow!("HEAD has no target commit"))?;
    let sha = oid.to_string();
    if sha.is_empty() {
        return Err(anyhow!("empty commit sha after sync"));
    }
    Ok(sha)
}

fn clone_repository(
    repo_url: &str,
    mirror_dir: &Path,
    branch: &str,
    git_token: Option<&str>,
) -> Result<()> {
    let callbacks = build_remote_callbacks(git_token);
    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);
    fetch_options.prune(git2::FetchPrune::On);
    fetch_options.download_tags(AutotagOption::None);

    let mut builder = RepoBuilder::new();
    builder.branch(branch);
    builder.fetch_options(fetch_options);
    builder
        .clone(repo_url, mirror_dir)
        .with_context(|| format!("git clone failed for {}", mirror_dir.display()))?;
    Ok(())
}

fn set_origin_url(repo: &Repository, repo_url: &str) -> Result<()> {
    match repo.find_remote("origin") {
        Ok(_) => repo
            .remote_set_url("origin", repo_url)
            .context("git remote set-url failed")?,
        Err(_) => {
            repo.remote("origin", repo_url)
                .context("git remote create origin failed")?;
        }
    }
    Ok(())
}

fn fetch_branch(repo: &Repository, branch: &str, git_token: Option<&str>) -> Result<()> {
    let callbacks = build_remote_callbacks(git_token);
    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);
    fetch_options.prune(git2::FetchPrune::On);
    fetch_options.download_tags(AutotagOption::None);

    let mut remote = repo
        .find_remote("origin")
        .context("git remote origin not found")?;
    remote
        .fetch(&[branch], Some(&mut fetch_options), None)
        .with_context(|| format!("git fetch origin {branch} failed"))?;
    Ok(())
}

fn hard_reset_to_origin_branch(repo: &Repository, branch: &str) -> Result<()> {
    let reference = repo
        .find_reference(&format!("refs/remotes/origin/{branch}"))
        .with_context(|| format!("origin branch ref not found: {branch}"))?;
    let commit = reference
        .peel_to_commit()
        .with_context(|| format!("failed resolving origin/{branch} to commit"))?;
    repo.reset(commit.as_object(), ResetType::Hard, None)
        .context("git reset --hard failed")?;
    Ok(())
}

fn clean_untracked(repo: &Repository) -> Result<()> {
    let mut status_options = StatusOptions::new();
    status_options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);

    let statuses = repo
        .statuses(Some(&mut status_options))
        .context("git status failed during cleanup")?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| anyhow!("repo has no workdir"))?;

    for entry in statuses.iter() {
        let status = entry.status();
        if !status.contains(Status::WT_NEW) {
            continue;
        }
        let Some(path) = entry.path() else {
            continue;
        };
        let absolute = workdir.join(path);
        if absolute.is_dir() {
            fs::remove_dir_all(&absolute)
                .with_context(|| format!("failed cleaning dir {}", absolute.display()))?;
        } else if absolute.exists() {
            fs::remove_file(&absolute)
                .with_context(|| format!("failed cleaning file {}", absolute.display()))?;
            remove_empty_parents_until_workdir(workdir, absolute.parent())?;
        }
    }
    Ok(())
}

fn remove_empty_parents_until_workdir(workdir: &Path, mut current: Option<&Path>) -> Result<()> {
    while let Some(dir) = current {
        if dir == workdir {
            break;
        }
        if !dir.exists() || !dir.is_dir() {
            current = dir.parent();
            continue;
        }
        if fs::read_dir(dir)
            .with_context(|| format!("failed listing dir {}", dir.display()))?
            .next()
            .is_some()
        {
            break;
        }
        fs::remove_dir(dir).with_context(|| format!("failed removing dir {}", dir.display()))?;
        current = dir.parent();
    }
    Ok(())
}

fn build_remote_callbacks(git_token: Option<&str>) -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    if let Some(token) = git_token {
        let token = token.to_string();
        callbacks.credentials(move |_url, _username_from_url, _allowed_types| {
            Cred::userpass_plaintext("x-access-token", &token)
        });
    }
    callbacks
}
