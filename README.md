# External Pod Autoscaler

[![CI](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/ci.yaml/badge.svg)](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/ci.yaml)
[![main](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/main.yaml/badge.svg)](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/main.yaml)
[![codecov](https://codecov.io/gh/ctxswitch/external-pod-autoscaler/graph/badge.svg)](https://codecov.io/gh/ctxswitch/external-pod-autoscaler)

A Kubernetes controller that scrapes Prometheus metrics directly from pods and manages HPAs to scale deployments based on those metrics.

## Install

```bash
kustomize build config/epa/base | kubectl apply -f -
```

## Usage

Create an `ExternalPodAutoscaler` resource:

```yaml
apiVersion: ctx.sh/v1beta1
kind: ExternalPodAutoscaler
metadata:
  name: my-app-scaler
  namespace: default
spec:
  minReplicas: 1
  maxReplicas: 10

  scrape:
    port: 8080
    interval: 15s

  scaleTargetRef:
    kind: Deployment
    name: my-app

  metrics:
  - metricName: requests_total
    targetValue: "100"
```

The controller will scrape the Prometheus metrics endpoint on your pods, aggregate the values over a sliding window, and create a native HPA to scale the target deployment.

If `scrapeTargetRef` is omitted it defaults to `scaleTargetRef`. Set it explicitly to scrape metrics from a different workload than the one being scaled.

```bash
kubectl get epa
kubectl describe epa my-app-scaler
```

## CRD Reference

| Field | Default | Description                                                                                                                               |
|---|---|-------------------------------------------------------------------------------------------------------------------------------------------|
| `minReplicas` | `1` | Minimum replica count                                                                                                                     |
| `maxReplicas` | *required* | Maximum replica count                                                                                                                     |
| `scrape.port` | `8080` | Metrics port                                                                                                                              |
| `scrape.path` | `/metrics` | Metrics path                                                                                                                              |
| `scrape.interval` | `15s` | Scrape interval                                                                                                                           |
| `scrape.timeout` | `1s` | Per-pod scrape timeout                                                                                                                    |
| `scrape.scheme` | `http` | `http` or `https`                                                                                                                         |
| `scrape.tls.insecureSkipVerify` | `false` | Skip TLS verification                                                                                                                     |
| `scrape.evaluationPeriod` | `60s` | Sliding window size                                                                                                                       |
| `scrape.aggregationType` | `avg` | `avg`, `max`, `min`, `median`, `last`                                                                                                     |
| `scrapeTargetRef` | *(scaleTargetRef)* | Workload whose pods are scraped for metrics.  This allows you to target other services for metrics (think producer/consumer relationship) |
| `scaleTargetRef` | *required* | Workload to scale                                                                                                                         |
| `metrics[].metricName` | *required* | Prometheus metric name                                                                                                                    |
| `metrics[].type` | `AverageValue` | `AverageValue` or `Value`                                                                                                                 |
| `metrics[].targetValue` | *required* | Target value                                                                                                                              |
| `metrics[].aggregationType` | *(from scrape)* | Per-metric override                                                                                                                       |
| `metrics[].evaluationPeriod` | *(from scrape)* | Per-metric override                                                                                                                       |
| `metrics[].labelSelector` | — | Filter by Prometheus labels                                                                                                               |
| `behavior` | — | Pass-through to HPA behavior spec                                                                                                         |

Durations accept humantime strings: `15s`, `1m`, `5m`, `1h`.

## Local Development

Requires [k3d](https://k3d.io/).

```bash
make localdev
make run
```

## Building

```bash
make build       # cargo build --release
make test        # cargo test
make clippy      # cargo clippy
make fmt-check   # cargo fmt --check
```

## License

See [LICENSE](LICENSE) file.
