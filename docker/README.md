# Anamnesis Docker Deployment

Templates for running the image behind a reverse proxy. The full guide —
image, compose profiles, volumes, health checks, environment — is
[docs/DOCKER.md](../docs/DOCKER.md).

## Files

- **nginx.conf.example** — nginx as a TLS reverse proxy in front of the
  server. CI runs it in front of the image and sends it a 2 MB hook event, a
  search and a health check.
- **.env.example** — the variables the server reads. A test fails the build
  when it names one nothing reads.
- **README.md** — this file.

The compose file is `docker-compose.yml` at the repository root, with a `prod`
and a `dev` profile.

## Quick Start

```bash
cp docker/.env.example docker/.env      # edit: at least ANAMNESIS_TOKEN
docker run -d --name anamnesis --env-file docker/.env \
  -p 127.0.0.1:8080:8080 -v anamnesis-data:/root/.anamnesis \
  ghcr.io/berketpbs/anamnesis:latest

# Is it recording? Exits non-zero when it would not be.
docker exec anamnesis anamnesis hook --probe
```

`--env-file` on `docker run` puts the file's variables in the container.
`docker compose --env-file` does not: it only fills in `${...}` in the compose
file, so with compose set them under the service's `environment:`.

## Behind nginx

```bash
mkdir -p docker/ssl
openssl req -x509 -newkey rsa:4096 -nodes -days 365 \
  -keyout docker/ssl/key.pem -out docker/ssl/cert.pem -subj "/CN=anamnesis.local"
cp docker/nginx.conf.example docker/nginx.conf
```

Run nginx on the same network as a container named `anamnesis`, with
`docker/nginx.conf` at `/etc/nginx/nginx.conf` and `docker/ssl` at
`/etc/nginx/ssl`. Hooks then point at the proxy with
`ANAMNESIS_SERVER=https://anamnesis.local`.

Three settings in the template are there because the server needs them:

- `client_max_body_size 16m` — the server reads hook events up to 16 MB, and
  nginx's default of 1 MB refuses a large tool output with 413.
- No rate limit on `/` — hooks arrive one per tool call, in bursts, and a hook
  refused by the proxy is an event lost.
- Only `Strict-Transport-Security` is added. The server sets its own
  `Content-Security-Policy`, `X-Frame-Options`, `X-Content-Type-Options` and
  `Referrer-Policy`; a second, different value is a conflict.

## One replica

The index is SQLite with a single writer. Do not scale the service past one
container on one volume.

## Where the data is

Inside the container, `/root/.anamnesis`:

- `db/anamnesis.db` — the index, rebuildable with `anamnesis reindex`
- `wiki/` — the pages, a git repository
- `raw/` — the transcripts
- `logs/` — the server's own log

`anamnesis backup` writes all of it (but `models/` and `logs/`) to one archive:

```bash
docker exec anamnesis anamnesis backup --out /root/.anamnesis/backup.tar.gz
```
