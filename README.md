# repo-sync

Rust HTTP file server that mirrors a private Git repository and serves files directly from the local mirror.

## What it does

- Clones/syncs a private repository on startup and on a configurable interval.
- Serves all files from the mirrored repository, or only from an optional subdirectory.
- Exposes metadata and health endpoints.
- Serves updated file content without restarting the server process.

## Endpoints

- `GET /health` - basic service and sync status.
- `GET /meta` - repo, branch, serve root, and sync metadata.
- `GET /files/*path` - fetch file bytes from mirrored repository.

## Configuration

Copy the example env file:

```bash
cp .env.example .env
```

Required:

- `GIT_REPO_URL` - source repository URL (private repo supported).

Optional:

- `GIT_BRANCH` (default `main`)
- `GIT_SYNC_INTERVAL_SECONDS` (default `30`)
- `GIT_TOKEN` (GitHub PAT for private HTTPS access)
- `MIRROR_DIR` (default `/data/repo`)
- `SERVE_SUBDIR` (optional path inside mirrored repo)
- `HTTP_BIND_ADDR` (default `0.0.0.0:8080`)
- `MAX_PATH_LENGTH` (default `512`)
- `MAX_FILE_SIZE_BYTES` (default `10485760`)

## Private repo auth

Recommended: use a read-only GitHub token.

- Create a fine-grained PAT with read-only `Contents` access to the source repo.
- Set it as `GIT_TOKEN`.
- Keep `GIT_REPO_URL` as standard HTTPS URL.

## Run locally

```bash
cargo run
```

Then test:

```bash
curl "http://localhost:8080/health"
curl "http://localhost:8080/meta"
curl "http://localhost:8080/files/path/in/repo/file.json"
```

## Docker / Compose

```bash
docker compose up -d --build
```

The compose file mounts a persistent volume for mirror data, so synced files survive container restarts.

## Notes

- JSON/file updates in the source repo are served after the next sync interval.
- Server restart is only needed for binary/config/image changes, not for source file updates.
