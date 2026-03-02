# External Pod Autoscaler

A Kubernetes controller that scrapes Prometheus metrics from pods and automatically manages HPAs to scale deployments based on those metrics. Supports distributed multi-replica operation with automatic work partitioning.

## Features

- **ExternalPodAutoscaler CRD** — single resource defines scraping + scaling configuration
- **Auto-managed HPAs** — controller creates, updates, and deletes native Kubernetes HPAs with ownerReferences
- **Flexible targeting** — scrape from any pods, scale any deployment (queue-worker pattern), or self-scale
- **Distributed scraping** — multi-replica deployment with rendezvous hashing for EPA ownership partitioning
- **Admission webhooks** — validating and mutating webhooks for CRD validation and defaults
- **Sliding window aggregation** — configurable evaluation periods (60s–10min) with avg, max, min, median, last
- **Per-metric overrides** — each metric can override aggregation type and evaluation period
- **Counter rate calculation** — automatic rate computation with reset detection
- **Label-based filtering** — filter Prometheus metrics by label selectors (matchLabels + matchExpressions)
- **HTTPS support** — TLS scraping with optional certificate verification
- **Namespace isolation** — all operations scoped to the EPA's namespace

## Quick Start

Requires [cert-manager](https://cert-manager.io/) for TLS certificates.

```bash
# Install cert-manager
kustomize build config/cert-manager | kubectl apply -f -

# Install controller
kustomize build config/epa/base | kubectl apply -f -
```

## Usage

### Queue-Worker Pattern

Scrape metrics from `queue-service` pods, scale `worker` deployment:

```yaml
apiVersion: ctx.sh/v1beta1
kind: ExternalPodAutoscaler
metadata:
  name: queue-worker-scaler
  namespace: production
spec:
  minReplicas: 2
  maxReplicas: 10

  scrape:
    port: 9090
    interval: 15s
    evaluationPeriod: 5m
    aggregationType: avg

  scrapeTargetRef:
    kind: Deployment
    name: queue-service

  scaleTargetRef:
    kind: Deployment
    name: worker

  metrics:
  - metricName: queue_depth
    type: AverageValue
    targetValue: "50"

  - metricName: queue_errors_total
    type: AverageValue
    targetValue: "5"
    evaluationPeriod: 2m
```

This creates an HPA named `queue-worker-scaler` targeting the `worker` deployment with external metrics `queue-worker-scaler-production-queue_depth` and `queue-worker-scaler-production-queue_errors_total`.

### Self-Scaling Pattern

Scrape and scale the same deployment (`scrapeTargetRef` defaults to `scaleTargetRef` when omitted):

```yaml
apiVersion: ctx.sh/v1beta1
kind: ExternalPodAutoscaler
metadata:
  name: api-scaler
  namespace: production
spec:
  minReplicas: 2
  maxReplicas: 10

  scrape:
    port: 8080
    scheme: https
    tls:
      insecureSkipVerify: true
    evaluationPeriod: 2m
    aggregationType: median

  scaleTargetRef:
    kind: Deployment
    name: api-server

  metrics:
  - metricName: http_requests_total
    type: AverageValue
    targetValue: "100"

  - metricName: http_request_duration_seconds
    type: AverageValue
    targetValue: "0.5"
    labelSelector:
      matchLabels:
        endpoint: /api/users
```

### Checking Status

```bash
# List EPAs
kubectl get epa

# Describe EPA (shows scrape status, HPA info, conditions)
kubectl describe epa queue-worker-scaler

# Check managed HPA
kubectl get hpa queue-worker-scaler

# Query external metrics directly
kubectl get --raw "/apis/external.metrics.k8s.io/v1beta1/namespaces/production/queue-worker-scaler-production-queue_depth"
```

## CRD Reference

### Spec

| Field | Type | Default | Description |
|---|---|---|---|
| `minReplicas` | int | `1` | Minimum replica count for HPA |
| `maxReplicas` | int | *required* | Maximum replica count for HPA |
| `scrape.port` | int | `8080` | Port to scrape metrics from |
| `scrape.path` | string | `/metrics` | HTTP path for metrics endpoint |
| `scrape.interval` | duration | `15s` | Scrape interval |
| `scrape.timeout` | duration | `1s` | Per-pod scrape timeout |
| `scrape.scheme` | string | `http` | `http` or `https` |
| `scrape.tls.insecureSkipVerify` | bool | `false` | Skip TLS certificate verification |
| `scrape.evaluationPeriod` | duration | `60s` | Sliding window size for aggregation |
| `scrape.aggregationType` | string | `avg` | Default aggregation: `avg`, `max`, `min`, `median`, `last` |
| `scrapeTargetRef.apiVersion` | string | `apps/v1` | API version of scrape target |
| `scrapeTargetRef.kind` | string | — | `Deployment`, `StatefulSet`, or `DaemonSet` |
| `scrapeTargetRef.name` | string | — | Name of scrape target (defaults to `scaleTargetRef` if omitted) |
| `scaleTargetRef.apiVersion` | string | `apps/v1` | API version of scale target |
| `scaleTargetRef.kind` | string | *required* | `Deployment`, `StatefulSet`, or `DaemonSet` |
| `scaleTargetRef.name` | string | *required* | Name of deployment to scale |
| `metrics[].metricName` | string | *required* | Prometheus metric name |
| `metrics[].type` | string | `AverageValue` | `AverageValue` or `Value` |
| `metrics[].targetValue` | string | *required* | Target value (must parse as float) |
| `metrics[].aggregationType` | string | *(from scrape)* | Per-metric aggregation override |
| `metrics[].evaluationPeriod` | duration | *(from scrape)* | Per-metric window override |
| `metrics[].labelSelector.matchLabels` | map | — | AND'd key=value label filter |
| `metrics[].labelSelector.matchExpressions` | list | — | `In`, `NotIn`, `Exists`, `DoesNotExist` |
| `behavior` | object | — | Pass-through to HPA `behavior` spec |

Duration fields accept humantime strings: `15s`, `1m`, `5m`, `1h`.

## External Metric Naming

Format: `{epa-name}-{epa-namespace}-{prometheus_metric_name}`

Underscores in the Prometheus metric name are preserved. Kubernetes resource names and namespaces use hyphens (never underscores), so the boundary is unambiguous.

| EPA Name | Namespace | Metric | External Metric Name |
|---|---|---|---|
| `queue-worker-scaler` | `production` | `queue_depth` | `queue-worker-scaler-production-queue_depth` |
| `queue-worker-scaler` | `production` | `queue_errors_total` | `queue-worker-scaler-production-queue_errors_total` |
| `api-scaler` | `staging` | `http_requests_total` | `api-scaler-staging-http_requests_total` |

## Architecture

```
┌────────────────────────────────────────────────────────────────────────┐
│ Controller Binary (runs as 3-replica Deployment)                       │
│                                                                        │
│  ┌──────────────┐  ┌───────────────────┐  ┌──────────────────────────┐ │
│  │ EPA          │  │ Scraper Service   │  │ Webhook/API Server       │ │
│  │ Controller   │──│ Worker pool (20)  │  │ HTTPS :8443              │ │
│  │              │  │ Hash-sharded EPAs │  │ - External Metrics API   │ │
│  │ Reconcile:   │  │ Pod discovery via │  │ - Validating webhook     │ │
│  │ - Manage HPA │  │   PodCache        │  │ - Mutating webhook       │ │
│  │ - Sync status│  │ Concurrent scrape │  │ - Cross-replica forward  │ │
│  └──────────────┘  └───────────────────┘  └──────────────────────────┘ │
│                                                                        │
│  ┌─────────────────────┐  ┌──────────────────────────────────────────┐ │
│  │ Membership Manager  │  │ MetricsStore                             │ │
│  │ Lease per replica   │  │ Sliding window per (EPA, metric, pod)   │ │
│  │ Rendezvous hashing  │  │ On-demand aggregation, 10s cache        │ │
│  └─────────────────────┘  └──────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────────────────────┘
```

**Distributed coordination**: Each replica creates a Kubernetes Lease renewed every 10s. EPA ownership is determined by rendezvous hashing across active replicas. External metrics requests are forwarded to the owning replica if received by a non-owner.

**Aggregation**: Two-stage process. Stage 1: per-pod sliding window aggregation (gauges use configured aggregation type; counters compute rate with reset detection). Stage 2: sum across all pods.

## Configuration

| Variable | Default | Description |
|---|---|---|
| `POD_UID` | *required* | Pod UID for replica identity (downward API) |
| `POD_IP` | *required* | Pod IP for request forwarding (downward API) |
| `POD_NAME` | *required* | Pod name for logging (downward API) |
| `POD_NAMESPACE` | *required* | Namespace for Lease creation (downward API) |
| `WEBHOOK_PORT` | `8443` | HTTPS server listen port |
| `ENABLE_WEBHOOKS` | `true` | Set `false` to disable admission webhooks |
| `SCRAPER_WORKERS` | `20` | Number of scraper worker tasks |
| `SERVICE_NAMESPACE` | `epa-system` | Namespace for APIService registration |
| `SERVICE_NAME` | `epa-webhook` | Service name for APIService registration |
| `LOG_FORMAT` | `text` | `json` for structured production logs |
| `RUST_LOG` | `external_pod_autoscaler=info,kube=info` | Log level filter |

## Local Development

Requires [k3d](https://k3d.io/) and [cert-manager](https://cert-manager.io/).

```bash
# Create k3d cluster, install cert-manager, deploy controller
make localdev

# Tail controller logs
make logs

# Shell into controller pod
make exec

# Delete cluster
make localdev-clean

# Delete and recreate
make localdev-restart

# Uninstall controller (keep cluster)
make localdev-uninstall
```

## Building

```bash
cargo build --release
cargo test
cargo clippy --all-targets --all-features
cargo fmt --check
```

Or via Makefile:

```bash
make build
make test
make clippy
make fmt-check
```

## License

See [LICENSE](LICENSE) file.
