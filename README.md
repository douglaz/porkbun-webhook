# Porkbun Webhook Provider for External-DNS

A high-performance Rust-based webhook provider that enables [external-dns](https://github.com/kubernetes-sigs/external-dns) to manage DNS records on [Porkbun](https://porkbun.com) domains.

## Features

- **Written in Rust** - Fast, safe, and memory-efficient
- **Full Porkbun API Support** - Complete integration with Porkbun's REST API
- **External-DNS Compatible** - Implements the official webhook provider specification
- **Idempotent Operations** - Safe under retries and partial failures
- **Production Ready** - Designed for Kubernetes deployments
- **Docker & Kubernetes Native** - Minimal container images with health checks
- **Domain Filtering** - Control which domains can be managed
- **Dry Run Mode** - Test changes without modifying DNS
- **Configurable Logging** - Structured logging with opt-in request body tracing

## Table of Contents

- [Quick Start](#quick-start)
- [Prerequisites](#prerequisites)
- [Installation](#installation)
- [Configuration](#configuration)
- [Kubernetes Deployment](#kubernetes-deployment)
- [External-DNS Integration](#external-dns-integration)
- [API Documentation](#api-documentation)
- [Troubleshooting](#troubleshooting)
- [Development](#development)

## Quick Start

```bash
# Run with Docker
docker run -d \
  -e PORKBUN_API_KEY=your-api-key \
  -e PORKBUN_SECRET_API_KEY=your-secret-key \
  -e DOMAIN_FILTER=yourdomain.com \
  -p 8888:8888 \
  ghcr.io/douglaz/porkbun-webhook:latest
```

## Prerequisites

1. **Porkbun API Key + Secret Key**: Generate at [porkbun.com/account/api](https://porkbun.com/account/api)
2. **API Access Enabled Per Domain**: In your Porkbun domain management page, enable "API Access" for each domain you want to manage. This is required even with valid API credentials.
3. **Kubernetes Cluster** (optional): For Kubernetes deployment

## Installation

### Using Docker

```bash
docker pull ghcr.io/douglaz/porkbun-webhook:latest

docker run -d \
  --name porkbun-webhook \
  -e PORKBUN_API_KEY=your-api-key \
  -e PORKBUN_SECRET_API_KEY=your-secret-key \
  -e DOMAIN_FILTER=example.com,example.org \
  -e WEBHOOK_HOST=0.0.0.0 \
  -e WEBHOOK_PORT=8888 \
  -e RUST_LOG=info \
  -p 8888:8888 \
  ghcr.io/douglaz/porkbun-webhook:latest
```

### Using Docker Compose

```bash
cp .env.example .env
# Edit .env with your API credentials
docker compose up -d
```

### From Source

```bash
git clone https://github.com/douglaz/porkbun-webhook
cd porkbun-webhook

# Using Cargo
cargo build --release
./target/release/porkbun-webhook

# Using Nix
nix build
./result/bin/porkbun-webhook
```

## Configuration

### Environment Variables

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `PORKBUN_API_KEY` | Your Porkbun API key from porkbun.com/account/api | - | Yes |
| `PORKBUN_SECRET_API_KEY` | Your Porkbun secret API key | - | Yes |
| `PORKBUN_API_BASE` | Porkbun API base URL | `https://api.porkbun.com/api/json/v3` | No |
| `WEBHOOK_HOST` | IP address to bind the webhook server | `127.0.0.1` | No |
| `WEBHOOK_PORT` | Port for the webhook server | `8888` | No |
| `DOMAIN_FILTER` | Comma-separated list of domains to manage | All domains | No |
| `DRY_RUN` | Enable dry-run mode (log changes without applying) | `false` | No |
| `CACHE_TTL_SECONDS` | Domain list cache TTL in seconds | `60` | No |
| `HTTP_TIMEOUT_SECONDS` | Timeout for Porkbun API requests | `30` | No |
| `TRACE_REQUEST_BODIES` | Log request bodies for debugging (opt-in) | `false` | No |
| `RUST_LOG` | Log level (trace, debug, info, warn, error) | `info` | No |

### Listener Model

This webhook uses a single listener on `WEBHOOK_HOST:WEBHOOK_PORT` (default `127.0.0.1:8888`) for both provider endpoints and health/readiness probes. The ExternalDNS documentation recommends provider traffic on `localhost:8888` with probes on `8080`, but this single-port model keeps the deployment simple.

## Kubernetes Deployment

### 1. Create Namespace and Secret

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: external-dns
---
apiVersion: v1
kind: Secret
metadata:
  name: porkbun-api-credentials
  namespace: external-dns
type: Opaque
stringData:
  api-key: "your-porkbun-api-key"
  secret-api-key: "your-porkbun-secret-api-key"
```

### 2. Deploy Porkbun Webhook

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: porkbun-webhook
  namespace: external-dns
spec:
  replicas: 1
  selector:
    matchLabels:
      app: porkbun-webhook
  template:
    metadata:
      labels:
        app: porkbun-webhook
    spec:
      containers:
      - name: porkbun-webhook
        image: ghcr.io/douglaz/porkbun-webhook:latest
        ports:
        - containerPort: 8888
        env:
        - name: PORKBUN_API_KEY
          valueFrom:
            secretKeyRef:
              name: porkbun-api-credentials
              key: api-key
        - name: PORKBUN_SECRET_API_KEY
          valueFrom:
            secretKeyRef:
              name: porkbun-api-credentials
              key: secret-api-key
        - name: WEBHOOK_HOST
          value: "0.0.0.0"
        - name: WEBHOOK_PORT
          value: "8888"
        - name: DOMAIN_FILTER
          value: "example.com"
        livenessProbe:
          httpGet:
            path: /healthz
            port: 8888
          initialDelaySeconds: 10
          periodSeconds: 30
        readinessProbe:
          httpGet:
            path: /ready
            port: 8888
          initialDelaySeconds: 5
          periodSeconds: 10
---
apiVersion: v1
kind: Service
metadata:
  name: porkbun-webhook
  namespace: external-dns
spec:
  selector:
    app: porkbun-webhook
  ports:
  - port: 8888
    targetPort: 8888
```

### 3. Deploy External-DNS

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: external-dns
  namespace: external-dns
spec:
  replicas: 1
  selector:
    matchLabels:
      app: external-dns
  template:
    metadata:
      labels:
        app: external-dns
    spec:
      serviceAccountName: external-dns
      containers:
      - name: external-dns
        image: registry.k8s.io/external-dns/external-dns:v0.14.0
        args:
        - --source=ingress
        - --source=service
        - --provider=webhook
        - --webhook-provider-url=http://porkbun-webhook:8888
        - --domain-filter=example.com
        - --registry=txt
        - --txt-owner-id=porkbun-webhook
        - --txt-prefix=_externaldns.
        - --interval=1m
```

See `examples/kubernetes/complete-deployment.yaml` for a full production manifest.

## External-DNS Integration

### How It Works

1. **External-DNS** watches for Kubernetes resources (Ingresses, Services) with DNS annotations
2. **External-DNS** detects changes and calls the webhook provider
3. **Porkbun Webhook** receives the changes and translates them to Porkbun API calls
4. **Porkbun API** updates the actual DNS records

### Supported Record Types

- **A** - IPv4 addresses
- **AAAA** - IPv6 addresses
- **CNAME** - Canonical names
- **TXT** - Text records (used for ownership)
- **MX** - Mail exchange (with priority via `providerSpecific`)
- **SRV** - Service records

### Priority for MX/SRV Records

Pass priority via the webhook `providerSpecific` metadata:

```json
{
  "dnsName": "example.com",
  "targets": ["mail.example.com"],
  "recordType": "MX",
  "providerSpecific": [
    { "name": "priority", "value": "10" }
  ]
}
```

## API Documentation

### Endpoints

| Endpoint | Method | Description | Response |
|----------|--------|-------------|----------|
| `/` | GET | Content-type negotiation and domain filters | `200 OK` |
| `/healthz` | GET | Liveness check (process-local) | `200 OK` |
| `/ready` | GET | Readiness check (pings Porkbun API) | `200 OK` |
| `/records` | GET | List DNS records | `200 OK` |
| `/records` | POST | Apply changes (create/update/delete) | `204 No Content` |
| `/adjustendpoints` | POST | Adjust endpoints (pass-through) | `200 OK` |

### Error Handling

- **4xx** responses indicate permanent errors (bad request, auth failure, domain not allowed)
- **5xx** responses indicate transient errors (upstream timeout, rate limiting, temporary Porkbun failure)
- ExternalDNS retries only on `5xx` responses

### GET /records

Query parameters:
- `zone` - Optional DNS zone to query

Response: JSON array of endpoints
```json
[
  {
    "dnsName": "app.example.com",
    "targets": ["192.168.1.1", "192.168.1.2"],
    "recordType": "A",
    "recordTTL": 600
  }
]
```

Multi-target records (e.g., round-robin A records) are grouped into a single endpoint with multiple targets.

### POST /records

Request body:
```json
{
  "create": [
    { "dnsName": "new.example.com", "targets": ["192.168.1.2"], "recordType": "A", "recordTTL": 300 }
  ],
  "updateOld": [],
  "updateNew": [],
  "delete": []
}
```

Returns `204 No Content` on success. Also accepts PascalCase keys (`Create`, `UpdateOld`, etc.) and wrapped payloads (`{ "changes": { ... } }`).

## Troubleshooting

### Common Issues

#### API Access Not Enabled

Porkbun requires API access to be enabled per domain. If you get authorization errors for specific domains, check your Porkbun dashboard and enable API access for each managed domain.

#### Authentication Failed

Verify your API credentials:
```bash
curl -X POST https://api.porkbun.com/api/json/v3/ping \
  -H "Content-Type: application/json" \
  -d '{"apikey":"your-key","secretapikey":"your-secret"}'
```

#### Records Not Created

Enable debug logging:
```bash
kubectl set env -n external-dns deployment/porkbun-webhook RUST_LOG=debug
```

#### Webhook Not Receiving Requests

Check external-dns can reach the webhook:
```bash
kubectl exec -n external-dns deployment/external-dns -- \
  wget -O- http://porkbun-webhook:8888/healthz
```

### Debugging Commands

```bash
# Check webhook logs
kubectl logs -n external-dns deployment/porkbun-webhook -f

# Check external-dns logs
kubectl logs -n external-dns deployment/external-dns -f

# List current records via webhook
curl http://localhost:8888/records?zone=example.com

# Test readiness
curl http://localhost:8888/ready
```

## Development

### Prerequisites

- Rust 1.70+ or Nix with flakes enabled
- Porkbun account with API access

### Local Development

```bash
git clone https://github.com/douglaz/porkbun-webhook
cd porkbun-webhook

cp .env.example .env
# Edit .env with your API credentials

cargo run

# Or with Nix
nix develop
cargo watch -x run
```

### Testing

```bash
cargo test
RUST_LOG=debug cargo test -- --nocapture
```

### Building

```bash
cargo build --release

# Docker image with Nix
nix build .#dockerImage
docker load < result
```

### Project Structure

```
porkbun-webhook/
├── src/
│   ├── main.rs           # Entry point
│   ├── lib.rs            # Library exports
│   ├── config.rs         # Configuration
│   ├── error.rs          # Error handling with transient classification
│   ├── middleware.rs      # Request logging (opt-in body tracing)
│   ├── porkbun/
│   │   ├── mod.rs        # Module definition
│   │   ├── client.rs     # Porkbun REST API client
│   │   └── types.rs      # API request/response types
│   └── webhook/
│       ├── mod.rs        # Module definition
│       ├── handlers.rs   # Request handlers with idempotent operations
│       ├── routes.rs     # Route setup
│       └── types.rs      # External-DNS types with record grouping
├── examples/kubernetes/  # Kubernetes manifests
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── flake.nix
└── .github/workflows/ci.yml
```

## Security

- **API Credentials**: Never logged or exposed in responses
- **TLS Only**: Porkbun API communication over HTTPS
- **Domain Filtering**: Restrict manageable domains
- **Opt-in Body Logging**: Request bodies only logged when explicitly enabled
- **No State Storage**: Stateless operation (domain list cached in-memory only)

## License

MIT License

## Acknowledgments

- [External-DNS](https://github.com/kubernetes-sigs/external-dns) team for the webhook specification
- [Porkbun](https://porkbun.com) for affordable domains with API access
