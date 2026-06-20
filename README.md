# gitea-mirror-sync

Keep **pull-mirror** repositories on a self-hosted **Gitea/Forgejo** instance in
sync with a source forge — and automatically pick up *new* repos as they appear.

Gitea/Forgejo can mirror a single repo, and once created it re-syncs the git data
on its own schedule. What it **cannot** do is "mirror every repo of a user/org and
keep discovering new ones". That's what this small daemon does:

- **Discovery & creation** — lists every repo of a user/org on the source and
  creates the missing pull mirrors on the target (one `migrate` call each).
- **Token rotation** — when the source token changes, re-authenticates every
  managed mirror (PATCH `mirror_token` on Gitea ≥ 1.27, else delete + recreate).
- **Respects the user** — if someone converts a mirror to a regular repo (or
  deletes it), it is **blacklisted** and never recreated.
- **Filters** — include/exclude by regex, quantity limit, skip forks/archived/private.

Sources supported: **GitHub** (incl. Enterprise), **GitLab** (incl. subgroups),
**Gitea/Forgejo**, **GitBucket**.

## How it works

```
            ┌─ discovery cron ─┐
startup ──▶ │  list target     │  detect broken mirrors → blacklist
            │  list source     │  create missing mirrors / adopt existing
            └──────────────────┘
startup only: token rotation (compare SHA-256 fingerprint of the source token)

actual git sync of each mirror = handled by Gitea/Forgejo (mirror_interval)
```

The cron is the **discovery / reconciliation** cadence, *not* the git-sync cadence.
The git sync of each mirror is done by the target instance itself via the mirror's
`mirror_interval`.

## Quick start

```sh
export GMS_TARGET_URL=https://gitea.example.com
export GMS_TARGET_TOKEN=...         # write:repository (+ create rights on the owner)
export GMS_TARGET_OWNER=my-org
export GMS_SOURCE_TYPE=github
export GMS_SOURCE_TOKEN=...         # PAT that can read the source repos
export GMS_SOURCE_OWNER=my-user-or-org

cargo run -- --check               # validate config + show detected target capabilities
cargo run -- --once                # one reconcile pass, then exit
cargo run                          # daemon: reconcile at startup, then on the cron schedule
```

Build a release binary with `cargo build --release` → `./target/release/gitea-mirror-sync`.

| Mode | Command | Use |
|------|---------|-----|
| Daemon | `gitea-mirror-sync` | Long-running; reconciles at startup then on `GMS_CRON`. |
| One-shot | `gitea-mirror-sync --once` | One pass (incl. token-rotation check), then exit. For an external scheduler (systemd timer, k8s CronJob). |
| Check | `gitea-mirror-sync --check` | Validate config + print detected capabilities, then exit. |

## Configuration

Everything is configured through `GMS_*` environment variables (a `.env` file in
the working directory is auto-loaded). A template is in [`.env.example`](.env.example).

### Target — the local Gitea/Forgejo that holds the mirrors

| Variable | Required | Default | Description |
|----------|:--------:|---------|-------------|
| `GMS_TARGET_URL` | **yes** | — | Base URL of your instance, e.g. `https://gitea.example.com`. |
| `GMS_TARGET_TOKEN` | **yes** | — | API token. Needs `write:repository` and permission to create repos under the owner (org owner / create-capable team, or site admin). |
| `GMS_TARGET_OWNER` | **yes** | — | User or org that will own the mirrors. |
| `GMS_TARGET_OWNER_TYPE` | no | `auto` | `auto` \| `user` \| `org`. `auto` probes the org endpoint to decide. |

### Source — the forge to mirror from

| Variable | Required | Default | Description |
|----------|:--------:|---------|-------------|
| `GMS_SOURCE_TYPE` | **yes** | — | `github` \| `gitlab` \| `gitea` \| `gitbucket`. |
| `GMS_SOURCE_URL` | conditional | `https://api.github.com` (github), `https://gitlab.com` (gitlab) | Base/API URL. **Required** for `gitea` and `gitbucket`. The per-forge suffix (`/api/v4`, `/api/v1`, `/api/v3`) is appended automatically if you give a bare host. |
| `GMS_SOURCE_TOKEN` | **yes** | — | Token used to list the source repos *and* embedded into each mirror so the target can clone. Use a read-scoped token. |
| `GMS_SOURCE_OWNER` | **yes** | — | User, org, or GitLab group (path or id) to mirror. |
| `GMS_SOURCE_OWNER_TYPE` | no | `auto` | `auto` \| `user` \| `org`. `auto` probes the org/group endpoint. |

### Scheduling

| Variable | Required | Default | Description |
|----------|:--------:|---------|-------------|
| `GMS_CRON` | no | `0 0 * * * *` | **6-field** cron (`sec min hour day-of-month month day-of-week`), in **UTC**. Discovery cadence. Default = hourly at :00. Ignored in `--once` mode. |

### Filters (applied to source repo **names**)

| Variable | Required | Default | Description |
|----------|:--------:|---------|-------------|
| `GMS_FILTER_INCLUDE` | no | *(none)* | Regex; only repos whose name matches are kept. |
| `GMS_FILTER_EXCLUDE` | no | *(none)* | Regex; repos whose name matches are dropped. |
| `GMS_FILTER_LIMIT` | no | *(none)* | Max number of repos to mirror (names are sorted first, so it's deterministic). |
| `GMS_SKIP_FORKS` | no | `false` | Skip forks. |
| `GMS_SKIP_ARCHIVED` | no | `false` | Skip archived repos. |
| `GMS_INCLUDE_PRIVATE` | no | `true` | Set `false` to mirror only public source repos. |

### Mirror behavior

| Variable | Required | Default | Description |
|----------|:--------:|---------|-------------|
| `GMS_MIRROR_INTERVAL` | no | `8h0m0s` | Gitea-side auto-sync interval (Go duration) set on each created mirror. |
| `GMS_MIRROR_PRIVATE` | no | `true` | Force every mirror private. Set `false` to follow the source repo's visibility. |
| `GMS_TRIGGER_SYNC` | no | `false` | Also POST `mirror-sync` for managed repos on each run (immediate sync). Off by default since Gitea syncs on its own interval. |
| `GMS_ROTATION_MODE` | no | `auto` | How to re-auth mirrors when the source token changes: `auto` (PATCH if supported, else delete+recreate) \| `recreate` \| `warn` (log only). |

### State & logging

| Variable | Required | Default | Description |
|----------|:--------:|---------|-------------|
| `GMS_STATE_FILE` | no | `gms-state.json` *(Docker image: `/data/gms-state.json`)* | Path to the JSON state file (managed list, blacklist, token fingerprint). |
| `RUST_LOG` | no | `info` | Log filter (standard `tracing` syntax), e.g. `info,gitea_mirror_sync=debug`. Not prefixed with `GMS_`. |

## Docker

The image is built `FROM scratch` — a fully static musl binary (rustls + ring),
with the root CA bundle compiled in (`webpki-roots`), so it needs no shared
libraries and no `/etc/ssl`. Result: a ~9 MB image with zero OS surface.

```sh
docker build -t gitea-mirror-sync .
docker run --rm \
  -e GMS_TARGET_URL=https://gitea.example.com -e GMS_TARGET_TOKEN=... \
  -e GMS_TARGET_OWNER=my-org \
  -e GMS_SOURCE_TYPE=github -e GMS_SOURCE_TOKEN=... -e GMS_SOURCE_OWNER=my-org \
  -v gms-state:/data \
  gitea-mirror-sync --once
```

The default state path inside the image is `/data/gms-state.json`; mount a volume
at `/data` to persist the managed list and blacklist across runs.

### Self-contained demo with docker-compose

[`docker-compose.yml`](docker-compose.yml) stands up a real Gitea instance and uses
it as **both** source and target, so you can try the whole flow offline:

```sh
docker compose build mirror-sync
docker compose up -d gitea          # wait until healthy

# minimal setup: an admin + token, a source org with a repo, and a target org
docker compose exec -u git gitea gitea admin user create \
  --admin --username admin --password 'Admin12345!' --email admin@example.com
TOKEN=$(docker compose exec -u git gitea gitea admin user \
  generate-access-token --username admin --scopes all --raw --token-name init)
api=http://localhost:3000/api/v1; H="Authorization: token $TOKEN"
curl -s -X POST "$api/orgs" -H "$H" -d '{"username":"srcorg"}'  -H 'Content-Type: application/json'
curl -s -X POST "$api/orgs" -H "$H" -d '{"username":"mirrors"}' -H 'Content-Type: application/json'
curl -s -X POST "$api/orgs/srcorg/repos" -H "$H" -H 'Content-Type: application/json' \
  -d '{"name":"hello-world","auto_init":true,"private":false}'

printf 'GMS_TARGET_TOKEN=%s\nGMS_SOURCE_TOKEN=%s\n' "$TOKEN" "$TOKEN" > .env
docker compose run --rm mirror-sync --once    # creates mirrors/hello-world
```

> The compose file sets `GITEA__migrations__ALLOW_LOCALNETWORKS=true` **only**
> because, in this demo, the source forge is on the same private Docker network
> (Gitea blocks migrations from private hosts by default as an anti-SSRF measure).
> You do **not** need this when mirroring from GitHub/GitLab/etc.

## CI & releases (GitHub Actions → GHCR)

- [`.github/workflows/ci.yml`](.github/workflows/ci.yml) — `cargo fmt` + `clippy`
  + tests on every push to `main` and on PRs.
- [`.github/workflows/release.yml`](.github/workflows/release.yml) — when you
  **publish a GitHub Release**, builds the image and pushes it to
  `ghcr.io/<owner>/<repo>` (multi-arch amd64 + arm64), tagged with the release
  version, `<major>.<minor>`, the tag name, and `latest`.

Use a semver tag (e.g. `v1.2.3`). No secrets to configure — it uses the built-in
`GITHUB_TOKEN` (`packages: write`). After the first push you can make the package
public from the repo's *Packages* settings, then `docker pull ghcr.io/<owner>/<repo>:latest`.

## Notes & caveats

- **Token rotation API**: updating an existing pull mirror's credentials only has
  a clean API on **Gitea ≥ 1.27** (`PATCH mirror_token`). On Forgejo and older
  Gitea the tool falls back to **delete + recreate** (a full re-clone; the repo
  briefly disappears). Detected automatically; override with `GMS_ROTATION_MODE`.
- **Blacklist**: a mirror the user converted to a regular repo *or* deleted is
  added to the blacklist and never recreated. To mirror it again, remove its entry
  from the state file.
- **GitHub/Gitea/GitBucket `user` sources**: private repos are listed only for the
  token owner's own account (via `/user/repos`); a *different* user exposes public
  repos only. Org sources list the private repos the token can see.
- **Permissions**: the target token's principal must be able to create repos in
  `GMS_TARGET_OWNER`.

## State file

```json
{
  "managed":   ["repo-a", "repo-b"],
  "blacklist": ["repo-the-user-unmirrored"],
  "token_fingerprint": "<sha256 of the source token>"
}
```

## License

MIT — see `Cargo.toml`.
