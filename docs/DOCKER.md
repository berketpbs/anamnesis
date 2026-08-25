# Docker Deployment Guide

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
  -v anamnesis-data:/root/.anamnesis \
  -v /path/to/workspace:/workspace \
  anamnesis:latest
```

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

# Build with custom base image
docker build --build-arg BASE_IMAGE=debian:bookworm .
```

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
| `ANTHROPIC_API_KEY` | — | Enables model-written consolidation. Without it, summaries are compiled by counting. |
| `ANAMNESIS_LLM_*` | see below | `PROVIDER`, `MODEL`, `BASE_URL`, `EFFORT`, `MAX_INPUT_TOKENS`, `MAX_OUTPUT_TOKENS`, `TIMEOUT_SECS`, `MAX_RETRIES`, `FALLBACKS` |
| `ANAMNESIS_EMBED_ENABLED` | unset | `1` turns on the local embedder, which downloads a model into `models/` on first use |

> `PORT` and `BIND` are **not** read. The entrypoint hardcodes
> `anamnesis serve --bind 0.0.0.0 --port 8080`; to change either, override the
> container command instead:
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

```bash
# Services on the same network can communicate by name
# e.g., anamnesis -> http://anamnesis:8080
docker-compose up
docker-compose exec postgres psql -h anamnesis -U user
```

## Health Checks

```bash
# Manual health check
docker exec anamnesis anamnesis status

# View health status
docker inspect anamnesis | grep -A 10 "Health"

# With curl
curl http://localhost:8080/health || echo "Unhealthy"
```

## Troubleshooting

### Build Issues

**Error: C++ build tools not found**
```bash
# Use vendored build
docker build --build-arg PROFILE=vendored .
```

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
```bash
# Fix ownership
docker exec anamnesis chown -R user:user /root/.anamnesis
```

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
- **Stage 1 (Builder)**: Compiles binary (1.5GB intermediate)
- **Stage 2 (Runtime)**: Only includes binary and runtime deps (~60MB)

### SQLite Performance

For high-concurrency scenarios, consider:
```bash
# Increase WAL checkpoints
docker run -e "SQLITE_CONFIG=wal_autocheckpoint=100000" anamnesis:latest
```

### Database Size

```bash
# Check database size
docker exec anamnesis du -sh /root/.anamnesis/

# Vacuum database
docker exec anamnesis sqlite3 /root/.anamnesis/memory.db VACUUM
```

## Kubernetes Deployment

### Helm Chart (Future)

```bash
# Install with Helm
helm install anamnesis ./helm

# Customize
helm install anamnesis ./helm \
  --set persistence.size=10Gi \
  --set replicaCount=3
```

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
