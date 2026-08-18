# Anamnesis Docker Deployment

This directory contains Docker configurations for running anamnesis in various environments.

## Files

- **docker-compose.prod.yml** - Production deployment with nginx reverse proxy
- **nginx.conf.example** - Nginx configuration template
- **.env.example** - Environment variables template
- **README.md** - This file

## Quick Start

### 1. Setup Environment

```bash
cd docker
cp .env.example .env
# Edit .env with your settings
```

### 2. Production Deployment

```bash
docker-compose -f docker-compose.prod.yml up -d
```

This starts:
- **anamnesis**: Main memory server on port 8080
- **nginx**: Reverse proxy on ports 80/443 (optional)

### 3. Verify Deployment

```bash
# Check status
docker-compose -f docker-compose.prod.yml exec anamnesis anamnesis status

# View logs
docker-compose -f docker-compose.prod.yml logs -f anamnesis

# Test API
curl http://localhost:8080/api/status
```

## Scaling

### Horizontal Scaling with Docker Swarm

```bash
# Initialize swarm
docker swarm init

# Deploy stack
docker stack deploy -c docker-compose.prod.yml anamnesis

# Scale service
docker service scale anamnesis_anamnesis=3
```

### Kubernetes

See the [Kubernetes deployment guide](../docs/DOCKER.md#kubernetes-deployment).

## Monitoring

### Health Checks

```bash
# Manual check
docker-compose -f docker-compose.prod.yml exec anamnesis \
  anamnesis status

# Automated monitoring (Prometheus)
curl http://localhost:8080/metrics
```

### Logs

```bash
# Follow logs
docker-compose -f docker-compose.prod.yml logs -f

# Filter by service
docker-compose -f docker-compose.prod.yml logs -f anamnesis

# Export logs
docker-compose -f docker-compose.prod.yml logs > logs.txt
```

### Metrics

```bash
# Check memory usage
docker stats anamnesis

# Check disk usage
docker exec anamnesis du -sh /data/anamnesis
```

## Backup & Restore

### Backup

```bash
# Backup database
docker-compose -f docker-compose.prod.yml exec anamnesis \
  tar czf - /data/anamnesis | gzip > anamnesis-backup.tar.gz

# Backup entire volume
docker run --rm \
  -v anamnesis-data:/data \
  -v $(pwd):/backup \
  alpine tar czf /backup/anamnesis-volume.tar.gz /data
```

### Restore

```bash
# Restore database
docker-compose -f docker-compose.prod.yml exec anamnesis \
  tar xzf - /data/anamnesis < anamnesis-backup.tar.gz

# Restore volume
docker run --rm \
  -v anamnesis-data:/data \
  -v $(pwd):/backup \
  alpine tar xzf /backup/anamnesis-volume.tar.gz -C /
```

## SSL/TLS Setup

### Self-Signed Certificates

```bash
# Generate certificate (valid for 365 days)
openssl req -x509 -newkey rsa:4096 -nodes -out cert.pem \
  -keyout key.pem -days 365 \
  -subj "/CN=anamnesis.local"

# Place in ssl directory
mkdir -p ssl
mv cert.pem key.pem ssl/
```

### Let's Encrypt

```bash
# Using certbot
certbot certonly --standalone -d anamnesis.example.com

# Copy certificates
cp /etc/letsencrypt/live/anamnesis.example.com/fullchain.pem ssl/cert.pem
cp /etc/letsencrypt/live/anamnesis.example.com/privkey.pem ssl/key.pem
```

## Network Setup

### Internal Network

Services communicate via the `anamnesis` network:
```bash
docker network inspect anamnesis
```

### External Access

Via nginx reverse proxy:
- **HTTP**: http://anamnesis.local (redirects to HTTPS)
- **HTTPS**: https://anamnesis.local
- **API**: https://anamnesis.local/api/

## Database

### SQLite (Default)

Located at `/data/anamnesis/memory.db`

```bash
# Access database
docker-compose -f docker-compose.prod.yml exec anamnesis \
  sqlite3 /data/anamnesis/memory.db

# Vacuum database
docker-compose -f docker-compose.prod.yml exec anamnesis \
  sqlite3 /data/anamnesis/memory.db "VACUUM;"
```

### PostgreSQL (Optional)

To enable PostgreSQL instead of SQLite:

```yaml
# In docker-compose.prod.yml
services:
  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_DB: anamnesis
      POSTGRES_USER: anamnesis
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
```

## Performance Tuning

### Container Resources

```yaml
deploy:
  resources:
    limits:
      cpus: '2'
      memory: 1G
    reservations:
      cpus: '1'
      memory: 512M
```

### Database Optimization

```bash
# Optimize SQLite
docker-compose exec anamnesis sqlite3 /data/anamnesis/memory.db << EOF
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA cache_size=-64000;
VACUUM;
ANALYZE;
EOF
```

## Troubleshooting

### Container Won't Start

```bash
# Check logs
docker-compose -f docker-compose.prod.yml logs anamnesis

# Verify volume
docker volume inspect anamnesis-data

# Clean and retry
docker-compose -f docker-compose.prod.yml down -v
docker-compose -f docker-compose.prod.yml up
```

### High Memory Usage

```bash
# Check process memory
docker stats --no-stream

# Reduce cache
docker exec anamnesis sqlite3 /data/anamnesis/memory.db \
  "PRAGMA cache_size=-4000;"
```

### Slow Queries

```bash
# Enable query logging
docker exec anamnesis \
  sqlite3 /data/anamnesis/memory.db \
  "PRAGMA query_only=0; SELECT * FROM pages LIMIT 10;"
```

## Cleanup

```bash
# Remove containers
docker-compose -f docker-compose.prod.yml down

# Remove data (⚠️ careful!)
docker-compose -f docker-compose.prod.yml down -v

# Remove images
docker rmi anamnesis:latest

# Clean all unused resources
docker system prune -a --volumes
```

## Related Documentation

- [Docker deployment guide](../docs/DOCKER.md)
- [Getting started](../docs/GETTING_STARTED.md)
- [Architecture](../docs/ARCHITECTURE.md)
