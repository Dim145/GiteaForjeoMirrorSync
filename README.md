# gitea-mirror-sync

Keep **pull-mirror** repositories on a self-hosted **Gitea/Forgejo** instance in
sync with a source forge — and automatically pick up *new* repos as they appear.

Gitea/Forgejo can mirror a single repo, and once created it re-syncs the git data
on its own schedule. What it **cannot** do is "mirror every repo of a user/org and
keep discovering new ones". That's what this daemon does:

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

The cron is the **discovery/reconciliation** cadence, *not* the git-sync cadence.
The git sync is done by the target instance itself via each mirror's
`mirror_interval` (configurable with `GMS_MIRROR_INTERVAL`).

## Configure

All config comes from `GMS_*` environment variables — see [`.env.example`](.env.example).
Minimum:

```sh
export GMS_TARGET_URL=https://gitea.example.com
export GMS_TARGET_TOKEN=...        # needs write:repository (+ org create / admin for the owner)
export GMS_TARGET_OWNER=my-org
export GMS_SOURCE_TYPE=github
export GMS_SOURCE_TOKEN=...        # PAT with repo read
export GMS_SOURCE_OWNER=my-user-or-org
```

## Run

```sh
cargo run -- --check      # validate config + print detected target capabilities
cargo run -- --once       # one reconcile pass (for an external scheduler) then exit
cargo run                 # daemon: reconcile at startup, then on the cron schedule
```

Build a release binary with `cargo build --release` (`./target/release/gitea-mirror-sync`).

### As an external cron / systemd timer

Run `gitea-mirror-sync --once` on your own schedule instead of the built-in daemon.
The startup token-rotation check runs on every `--once` invocation.

## Docker

The image is built `FROM scratch` — a fully static musl binary (rustls + ring),
with the root CA bundle compiled in (`webpki-roots`), so it needs no shared
libraries and no `/etc/ssl`. Result: a ~9 MB image with zero OS surface.

```sh
docker build -t gitea-mirror-sync .
docker run --rm gitea-mirror-sync --help
```

A [`docker-compose.yml`](docker-compose.yml) is included that stands up a real
Gitea instance and uses it as **both** the source and the target, so you can try
the whole flow self-contained:

```sh
docker compose build mirror-sync
docker compose up -d gitea
# create an admin + token, an org "srcorg" with a couple of repos, and an org
# "mirrors" (see the steps below), put the token in a .env file, then:
docker compose run --rm mirror-sync --once     # one pass
docker compose up -d mirror-sync               # or run as a daemon
```

Minimal self-contained setup (after `gitea` is healthy):

```sh
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
docker compose run --rm mirror-sync --once
```

> The compose file sets `GITEA__migrations__ALLOW_LOCALNETWORKS=true` **only**
> because, in this test, the source forge is on the same private Docker network
> (Gitea blocks migrations from private hosts by default as an anti-SSRF measure).
> You do **not** need this when mirroring from GitHub/GitLab/etc.

## Releases (GitHub Actions → GHCR)

Two workflows are included:

- [`.github/workflows/ci.yml`](.github/workflows/ci.yml) — runs `cargo fmt`,
  `clippy` and the tests on every push to `main` and on PRs.
- [`.github/workflows/release.yml`](.github/workflows/release.yml) — when you
  **publish a GitHub Release**, it builds the image and pushes it to
  `ghcr.io/<owner>/<repo>` (multi-arch amd64 + arm64), tagged with the release
  version, `<major>.<minor>`, the tag name, and `latest`.

Use a semver tag (e.g. `v1.2.3`) for the release so the version tags are derived
correctly. No secrets to configure — the workflow uses the built-in
`GITHUB_TOKEN` (`packages: write`). After the first push you can make the package
public from the repo's *Packages* settings, then:

```sh
docker pull ghcr.io/<owner>/<repo>:latest
```

## Notes & caveats

- **Token rotation API**: updating an existing pull mirror's credentials only has
  a clean API on **Gitea ≥ 1.27** (`PATCH mirror_token`). On Forgejo and older
  Gitea the tool falls back to **delete + recreate** (a full re-clone; the repo
  briefly disappears). Detected automatically; override with `GMS_ROTATION_MODE`.
- **Blacklist**: stored in the state file (`GMS_STATE_FILE`). To re-mirror a repo
  that was blacklisted, remove its entry from that file.
- **GitHub private repos**: for *user* sources, only the token owner's own private
  repos are listed (via `/user/repos`); other users expose public repos only.
  Org sources list private repos the token can see.
- **Permissions**: the target token's principal must be able to create repos in
  `GMS_TARGET_OWNER` (org owner / create-capable team, or site admin).

## State file

```json
{
  "managed":   ["repo-a", "repo-b"],
  "blacklist": ["repo-the-user-unmirrored"],
  "token_fingerprint": "<sha256 of the source token>"
}
```
