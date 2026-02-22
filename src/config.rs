use std::{env, path::PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::path_guard::normalize_relative_path;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub git_repo_url: String,
    pub git_branch: String,
    pub git_sync_interval_seconds: u64,
    pub git_token: Option<String>,
    pub mirror_dir: PathBuf,
    pub serve_subdir: Option<PathBuf>,
    pub http_bind_addr: String,
    pub max_path_length: usize,
    pub max_file_size_bytes: u64,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        let git_repo_url = required("GIT_REPO_URL")?;
        let git_branch = optional("GIT_BRANCH").unwrap_or_else(|| "main".to_string());
        let git_sync_interval_seconds = optional("GIT_SYNC_INTERVAL_SECONDS")
            .as_deref()
            .unwrap_or("30")
            .parse::<u64>()
            .context("GIT_SYNC_INTERVAL_SECONDS must be an integer")?;
        let git_token = optional("GIT_TOKEN");
        let mirror_dir =
            PathBuf::from(optional("MIRROR_DIR").unwrap_or_else(|| "/data/repo".to_string()));
        let serve_subdir = optional("SERVE_SUBDIR")
            .map(|v| normalize_relative_path(&v))
            .transpose()
            .context("SERVE_SUBDIR must be a safe relative path")?
            .map(PathBuf::from);
        let http_bind_addr =
            optional("HTTP_BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8080".to_string());
        let max_path_length = optional("MAX_PATH_LENGTH")
            .as_deref()
            .unwrap_or("512")
            .parse::<usize>()
            .context("MAX_PATH_LENGTH must be an integer")?;
        let max_file_size_bytes = optional("MAX_FILE_SIZE_BYTES")
            .as_deref()
            .unwrap_or("10485760")
            .parse::<u64>()
            .context("MAX_FILE_SIZE_BYTES must be an integer")?;

        if git_sync_interval_seconds == 0 {
            return Err(anyhow!("GIT_SYNC_INTERVAL_SECONDS must be > 0"));
        }
        if max_path_length == 0 {
            return Err(anyhow!("MAX_PATH_LENGTH must be > 0"));
        }

        Ok(Self {
            git_repo_url,
            git_branch,
            git_sync_interval_seconds,
            git_token,
            mirror_dir,
            serve_subdir,
            http_bind_addr,
            max_path_length,
            max_file_size_bytes,
        })
    }

    pub fn serve_root(&self) -> PathBuf {
        match &self.serve_subdir {
            Some(subdir) => self.mirror_dir.join(subdir),
            None => self.mirror_dir.clone(),
        }
    }

    pub fn repo_url_with_auth(&self) -> String {
        match (&self.git_token, self.git_repo_url.strip_prefix("https://")) {
            (Some(token), Some(rest)) => format!("https://x-access-token:{token}@{rest}"),
            _ => self.git_repo_url.clone(),
        }
    }
}

fn required(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("Missing required env var: {key}"))
}

fn optional(key: &str) -> Option<String> {
    env::var(key).ok().and_then(|v| {
        let trimmed = v.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}
