use std::{process::Command, sync::Arc};

use repo_sync::{
    config::AppConfig,
    sync::{SyncStatus, sync_once},
};
use tempfile::tempdir;
use tokio::sync::RwLock;

fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    // When tests run under git hooks, inherited GIT_* vars can make commands
    // target the outer repository instead of this temp repository.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
        "GIT_CEILING_DIRECTORIES",
    ] {
        cmd.env_remove(key);
    }
    let output = cmd.output().expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn sync_once_updates_mirror_when_source_changes() {
    let tmp = tempdir().expect("temp dir");
    let source = tmp.path().join("source");
    let mirror = tmp.path().join("mirror");
    std::fs::create_dir_all(&source).expect("create source dir");

    run_git(&source, &["init"]);
    run_git(&source, &["checkout", "-b", "main"]);
    run_git(&source, &["config", "user.email", "bot@example.com"]);
    run_git(&source, &["config", "user.name", "Bot"]);
    run_git(&source, &["config", "commit.gpgsign", "false"]);

    std::fs::write(source.join("collections.json"), "{\"version\":1}").expect("write v1");
    run_git(&source, &["add", "."]);
    run_git(&source, &["commit", "-m", "v1"]);

    let config = AppConfig {
        git_repo_url: format!("file://{}", source.display()),
        git_branch: "main".to_string(),
        git_sync_interval_seconds: 30,
        git_token: None,
        mirror_dir: mirror.clone(),
        serve_subdir: None,
        http_bind_addr: "127.0.0.1:0".to_string(),
        max_path_length: 512,
        max_file_size_bytes: 1024 * 1024,
    };
    let status = Arc::new(RwLock::new(SyncStatus::default()));

    sync_once(&config, status.clone())
        .await
        .expect("first sync should work");
    let current_after_first = status.read().await.current_sha.clone();
    assert!(current_after_first.is_some());
    assert_eq!(
        std::fs::read_to_string(mirror.join("collections.json")).expect("read mirrored file"),
        "{\"version\":1}"
    );

    std::fs::write(source.join("collections.json"), "{\"version\":2}").expect("write v2");
    run_git(&source, &["add", "."]);
    run_git(&source, &["commit", "-m", "v2"]);

    sync_once(&config, status.clone())
        .await
        .expect("second sync should work");

    let status_snapshot = status.read().await.clone();
    assert!(status_snapshot.current_sha.is_some());
    assert_ne!(status_snapshot.current_sha, current_after_first);
    assert!(status_snapshot.previous_sha.is_some());
    assert_eq!(
        std::fs::read_to_string(mirror.join("collections.json")).expect("read mirrored file"),
        "{\"version\":2}"
    );
}

#[tokio::test]
async fn sync_once_removes_untracked_files_and_dirs_from_mirror() {
    let tmp = tempdir().expect("temp dir");
    let source = tmp.path().join("source");
    let mirror = tmp.path().join("mirror");
    std::fs::create_dir_all(&source).expect("create source dir");

    run_git(&source, &["init"]);
    run_git(&source, &["checkout", "-b", "main"]);
    run_git(&source, &["config", "user.email", "bot@example.com"]);
    run_git(&source, &["config", "user.name", "Bot"]);
    run_git(&source, &["config", "commit.gpgsign", "false"]);

    std::fs::create_dir_all(source.join("nested")).expect("create source nested dir");
    std::fs::write(source.join("collections.json"), "{\"version\":1}").expect("write v1");
    std::fs::write(source.join("nested/tracked.json"), "{\"tracked\":true}").expect("write nested");
    run_git(&source, &["add", "."]);
    run_git(&source, &["commit", "-m", "initial"]);

    let config = AppConfig {
        git_repo_url: format!("file://{}", source.display()),
        git_branch: "main".to_string(),
        git_sync_interval_seconds: 30,
        git_token: None,
        mirror_dir: mirror.clone(),
        serve_subdir: None,
        http_bind_addr: "127.0.0.1:0".to_string(),
        max_path_length: 512,
        max_file_size_bytes: 1024 * 1024,
    };
    let status = Arc::new(RwLock::new(SyncStatus::default()));

    sync_once(&config, status.clone())
        .await
        .expect("first sync should work");

    let untracked_file = mirror.join("local-only.json");
    let untracked_dir = mirror.join("local-cache");
    let untracked_nested_file = untracked_dir.join("cache.json");
    std::fs::write(&untracked_file, "{\"ephemeral\":true}").expect("write untracked file");
    std::fs::create_dir_all(&untracked_dir).expect("create untracked dir");
    std::fs::write(&untracked_nested_file, "{\"ephemeral\":true}").expect("write untracked nested");

    sync_once(&config, status)
        .await
        .expect("second sync should work");

    assert!(!untracked_file.exists());
    assert!(!untracked_dir.exists());
    assert!(!untracked_nested_file.exists());
    assert_eq!(
        std::fs::read_to_string(mirror.join("collections.json")).expect("read mirrored file"),
        "{\"version\":1}"
    );
    assert_eq!(
        std::fs::read_to_string(mirror.join("nested/tracked.json"))
            .expect("read mirrored nested file"),
        "{\"tracked\":true}"
    );
}
