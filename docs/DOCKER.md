# Docker Deployment Guide

## From the registry

Tagged versions are published to the GitHub container registry for both
`linux/amd64` and `linux/arm64` — Docker picks the one this machine needs:

```bash
docker pull ghcr.io/berketpbs/anamnesis:latest
docker run --rm ghcr.io/berketpbs/anamnesis:latest anamnesis --version
```

Everything below works the same against a pulled image as against one built
here; replace `anamnesis:latest` with `ghcr.io/berketpbs/anamnesis:latest`.

Anamnesis includes complete Docker support for local development and production deployment.

## Quick Start

### Development Mode (Hot Reload)

```bash
# Start development environment with cargo watch
docker-compose --profile dev up anamnesis-dev

# CLI usage
docker-compose --profile dev exec anamnesis-dev cargo run -p anamnesis-cli -- status
```

### Production Mode

```bash
# Build production image
docker build -t anamnesis:latest .

# Run production container
docker run -d \
  --name anamnesis \
  -p 8080:8080 \
  -e ANAMNESIS_TOKEN="$(anamnesis token)" \
  -v anamnesis-data:/root/.anamnesis \
  -v /path/to/workspace:/workspace \
  anamnesis:latest
```

### Requiring a token

`-p 8080:8080` publishes the port on every interface of the host, and
everything behind it — every prompt, every path, every summary written from
them — is readable by whatever can reach it. `ANAMNESIS_TOKEN` closes that:
with it set, every route but `/health` requires `Authorization: Bearer <token>`.
Whatever runs the hooks sets the same value in its own environment.

Without the variable the image starts open and says so on stderr. It does not
refuse, the way `anamnesis serve` refuses a non-loopback bind on the host,
because a container binds `0.0.0.0` in order to be reachable at all — the
decision that matters is made outside it, when the port is published. The other
way to close it is to publish to loopback only:
`-p 127.0.0.1:8080:8080`.

## Docker Compose Services

### anamnesis-dev
- Profile: `dev`
- Purpose: Development with hot reload
- Features:
  - `cargo watch` for automatic rebuilds
  - Source code mounted as volume
  - Debug logging enabled
  - Port 8080 exposed

**Usage:**
```bash
docker-compose --profile dev up anamnesis-dev

# In another terminal
docker-compose --profile dev exec anamnesis-dev cargo test
docker-compose --profile dev exec anamnesis-dev cargo clippy
```

### anamnesis
- Profile: `prod`
- Purpose: Production deployment
- Features:
  - Multi-stage build (60MB image)
  - Health checks
  - Auto-restart on failure
  - Persistent data volume

**Usage:**
```bash
docker-compose --profile prod up anamnesis

# Check status
docker-compose --profile prod exec anamnesis anamnesis status
```

### postgres (Optional, and unused)
- Profile: `postgres`
- Purpose: reserved for a future PostgreSQL backend
- Version: 16 Alpine

> **Anamnesis does not talk to PostgreSQL.** Storage is SQLite, bundled into
> the binary, living in the data directory. Starting this profile gives you an
> empty database that nothing writes to.

**Usage:**
```bash
docker-compose --profile postgres up postgres

# Connect
psql -h localhost -U anamnesis -d anamnesis
```

## Building Images

### Production Image

```bash
# Build with default tag
docker build -t anamnesis:latest .

# Build with specific version
docker build -t anamnesis:0.1.0 .
```

The Dockerfile takes no build arguments. Both stages name Debian trixie on
purpose: a binary linked against the builder's glibc does not start on an older
runtime, and nothing about that shows while building.

### Development Image

```bash
# Build development image
docker build -f Dockerfile.dev -t anamnesis:dev .

# Run with code mounted
docker run -it \
  -v $(pwd):/app \
  -v anamnesis-data:/app/data \
  anamnesis:dev
```

## Volume Management

### Persistent Data

```bash
# Create named volume
docker volume create anamnesis-data

# Inspect volume
docker volume inspect anamnesis-data

# Backup volume
docker run --rm \
  -v anamnesis-data:/data \
  -v $(pwd):/backup \
  alpine tar czf /backup/anamnesis-data.tar.gz /data

# Restore volume
docker run --rm \
  -v anamnesis-data:/data \
  -v $(pwd):/backup \
  alpine tar xzf /backup/anamnesis-data.tar.gz -C /
```

### Workspace Mount

Mount your project directory as `/workspace`:

```bash
docker run -d \
  -v /path/to/my-project:/workspace \
  anamnesis:latest
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ANAMNESIS_DATA_DIR` | `/root/.anamnesis` | Data directory root (`wiki/`, `raw/`, `db/`, `models/`, `logs/`) |
| `RUST_LOG` | `info` | Logging level (debug, info, warn, error) |
| `ANTHROPIC_API_KEY` | — | Enables model-written consolidation with Anthropic. Without a model, summaries are compiled by counting. |
| `ANAMNESIS_LLM_*` | — | `PROVIDER`, `MODEL`, `API_KEY`, `BASE_URL`, `EFFORT`, `MAX_INPUT_TOKENS`, `MAX_OUTPUT_TOKENS`, `TIMEOUT_SECS`, `MAX_RETRIES`, `FALLBACKS`, `FALLBACK_PROVIDERS` |
| `ANAMNESIS_EMBED_*` | unset | `ENABLED=1` turns vectors on; `PROVIDER`, `MODEL`, `URL`, `API_KEY` pick the embedder. The local one downloads a model into `models/` on first use |
| `ANAMNESIS_TOKEN` / `ANAMNESIS_TOKENS` | unset | Require a bearer token on every route but `/health` |

A container has no credential store, so a key goes in the environment
(`-e`, or an `--env-file` readable only by you), never in `settings.env`.

> `PORT` and `BIND` are **not** read. The image's command is
> `anamnesis serve --bind 0.0.0.0 --port 8080 --allow-anonymous`; to change
> either, override the container command instead:
>
> ```bash
> docker run anamnesis:latest anamnesis serve --bind 0.0.0.0 --port 9000
> ```

**Usage:**
```bash
docker run -e RUST_LOG=debug anamnesis:latest
```

## Networking

### Access from Host

```bash
# Container listens on 0.0.0.0:8080
# Access from host: http://localhost:8080
curl http://localhost:8080/health
```

### Inter-container Communication

Services on the same compose network reach the server by its service name, at
`http://anamnesis:8080`. A hook running in another container points there with
`ANAMNESIS_SERVER=http://anamnesis:8080`.

## Health Checks

The image's `HEALTHCHECK` runs `anamnesis hook --probe`: it sends the server
the event a hook would, asks it to record nothing, and exits non-zero when
memory would not be recorded. It presents `ANAMNESIS_TOKEN` when the container
has one.

```bash
# The same check by hand, and what it found
docker exec anamnesis anamnesis hook --probe

# What Docker has concluded
docker inspect --format '{{.State.Health.Status}}' anamnesis

# Liveness only, from the host
curl -fsS http://localhost:8080/health
```

`anamnesis status` describes the server and always exits 0, so it is the
command to read and not one to check.

## Troubleshooting

### Build Issues

**Error: Link failure on Windows**
```bash
# Use WSL2 backend for Docker
# In Docker Desktop settings: Backend = WSL 2
```

### Runtime Issues

**Container exits immediately**
```bash
# Check logs
docker logs anamnesis

# Run with interactive terminal
docker run -it anamnesis:latest /bin/bash
```

**Permission denied in volume**

The server runs as root in the container and writes under `/root/.anamnesis`.
A bind mount from the host has the host's ownership; a named volume
(`-v anamnesis-data:/root/.anamnesis`) avoids the question.

**Out of disk space**
```bash
# Clean up Docker resources
docker system prune -a

# Remove specific volume
docker volume rm anamnesis-data
```

## Performance Tuning

### Multi-stage Build Optimization

The production Dockerfile uses multi-stage builds:
- **Stage 1 (Builder)**: compiles the binary
- **Stage 2 (Runtime)**: Debian trixie slim with the binary, `sqlite3`,
  `ca-certificates`, `tini` and `git`

### Database Size

```bash
# Check data directory size
docker exec anamnesis du -sh /root/.anamnesis/

# Vacuum the index (stop writes first: the server holds it open)
docker exec anamnesis sqlite3 /root/.anamnesis/db/anamnesis.db VACUUM
```

The index is `db/anamnesis.db`. A command pointed at any other file name gets
a new, empty database and reports success.

## Kubernetes Deployment

There is no Helm chart. Run **one** replica: the index is SQLite with a single
writer, and two pods on one volume would contend for it.

### Manual Deployment

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: anamnesis
spec:
  containers:
  - name: anamnesis
    image: anamnesis:latest
    ports:
    - containerPort: 8080
    env:
    - name: RUST_LOG
      value: "info"
    volumeMounts:
    - name: data
      mountPath: /root/.anamnesis
  volumes:
  - name: data
    persistentVolumeClaim:
      claimName: anamnesis-pvc
```

## Best Practices

1. **Use specific image tags** - Avoid `latest` in production
2. **Enable health checks** - Monitor container health
3. **Mount volumes** - Don't lose data between restarts
4. **Set resource limits** - Prevent resource exhaustion
5. **Use read-only mounts** - For workspace/projects
6. **Configure logging** - Aggregate logs to ELK/Loki

## Example: Full Stack

```bash
# Start full development stack
docker-compose --profile dev --profile postgres up -d

# Watch logs
docker-compose logs -f

# Run tests
docker-compose --profile dev exec anamnesis-dev cargo test

# Access database
docker-compose --profile postgres exec postgres \
  psql -U anamnesis -d anamnesis

# Cleanup
docker-compose --profile dev --profile postgres down -v
```

## CI/CD Integration

### GitHub Actions

```yaml
- name: Build Docker image
  run: docker build -t anamnesis:${{ github.sha }} .

- name: Push to registry
  run: |
    docker tag anamnesis:${{ github.sha }} ghcr.io/berketpbs/anamnesis:latest
    docker push ghcr.io/berketpbs/anamnesis:latest
```

### GitLab CI

```yaml
docker-build:
  stage: build
  image: docker:latest
  services:
    - docker:dind
  script:
    - docker build -t $CI_REGISTRY_IMAGE:$CI_COMMIT_SHA .
    - docker push $CI_REGISTRY_IMAGE:$CI_COMMIT_SHA
```
