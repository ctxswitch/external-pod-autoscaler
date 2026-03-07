# External Pod Autoscaler

[![CI](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/ci.yaml/badge.svg)](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/ci.yaml)
[![main](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/main.yaml/badge.svg)](https://github.com/ctxswitch/external-pod-autoscaler/actions/workflows/main.yaml)
[![codecov](https://codecov.io/gh/ctxswitch/external-pod-autoscaler/graph/badge.svg)](https://codecov.io/gh/ctxswitch/external-pod-autoscaler)

A Kubernetes controller that scrapes Prometheus metrics directly from pods and manages HPAs to scale deployments based on those metrics.

## Install

```bash
helm repo add epa https://ctxswitch.github.io/external-pod-autoscaler
helm install epa epa/external-pod-autoscaler --namespace epa-system --create-namespace
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

The controller scrapes Prometheus metrics directly from pods, aggregates values over a sliding window, and creates a native HPA to scale the target deployment.

### Scraping from a different workload

By default the controller scrapes metrics from the same workload it scales (`scaleTargetRef`). Set `scrapeTargetRef` to decouple them — scrape metrics from one workload while scaling another. This is useful for producer/consumer patterns where the scaling signal lives on a different service than the one being scaled:

```yaml
spec:
  scaleTargetRef:
    kind: Deployment
    name: consumer         # workload to scale
  scrapeTargetRef:
    kind: Deployment
    name: producer         # workload to scrape metrics from
  metrics:
    - metricName: queue_depth
      targetValue: "100"
```

## CRD Reference

All duration fields accept humantime strings: `15s`, `1m`, `5m`, `1h`.

### `spec`

| Field | Type | Default | Description |
|---|---|---|---|
| `minReplicas` | int | `1` | Minimum replica count |
| `maxReplicas` | int | **required** | Maximum replica count |
| `scrape` | [ScrapeConfig](#scrape) | — | Metrics scraping configuration |
| `scaleTargetRef` | [TargetRef](#targetref) | **required** | Workload to scale |
| `scrapeTargetRef` | [TargetRef](#targetref) | *(scaleTargetRef)* | Workload to scrape metrics from. Defaults to `scaleTargetRef` if omitted. Set this to scrape a different workload than the one being scaled (e.g. a producer/consumer pattern). |
| `metrics` | [\[\]MetricSpec](#metrics) | **required** | Metrics to collect and use for scaling decisions |
| `behavior` | [Behavior](#behavior) | — | HPA scaling behavior (pass-through to HPA `spec.behavior`) |

### `scrape`

| Field | Type | Default | Description |
|---|---|---|---|
| `port` | int | `8080` | Port to scrape metrics from |
| `path` | string | `/metrics` | HTTP path for the metrics endpoint |
| `interval` | duration | `15s` | How often to scrape each pod |
| `timeout` | duration | `1s` | Per-pod scrape timeout |
| `scheme` | string | `http` | `http` or `https` |
| `evaluationPeriod` | duration | `60s` | Sliding window over which samples are aggregated |
| `aggregationType` | string | `avg` | `avg`, `max`, `min`, `median`, `last` |
| `tls.insecureSkipVerify` | bool | `false` | Skip TLS certificate verification (only applies when `scheme: https`) |

### `targetRef`

Used by both `scaleTargetRef` and `scrapeTargetRef`.

| Field | Type | Default | Description |
|---|---|---|---|
| `apiVersion` | string | `apps/v1` | API version of the target resource |
| `kind` | string | **required** | `Deployment`, `StatefulSet`, etc. |
| `name` | string | **required** | Name of the target resource |

### `metrics[]`

| Field | Type | Default | Description |
|---|---|---|---|
| `metricName` | string | **required** | Prometheus metric name to scrape |
| `type` | string | `AverageValue` | `AverageValue` (divided by replica count) or `Value` (raw) |
| `targetValue` | string | **required** | Scaling threshold |
| `aggregationType` | string | *(from scrape)* | Per-metric aggregation override |
| `evaluationPeriod` | duration | *(from scrape)* | Per-metric evaluation window override |
| `labelSelector` | [LabelSelector](#labelselector) | — | Filter scraped samples by Prometheus labels |

### `labelSelector`

| Field | Type | Description |
|---|---|---|
| `matchLabels` | map[string]string | Key-value pairs, AND'd together |
| `matchExpressions[]` | list | Expression-based requirements, AND'd together |
| `matchExpressions[].key` | string | Label key |
| `matchExpressions[].operator` | string | `In`, `NotIn`, `Exists`, `DoesNotExist` |
| `matchExpressions[].values` | []string | Required for `In`/`NotIn` |

### `behavior`

Pass-through to the [HPA behavior spec](https://kubernetes.io/docs/tasks/run-application/horizontal-pod-autoscale/#configurable-scaling-behavior).

| Field | Type | Description |
|---|---|---|
| `scaleUp` | ScalingRules | Rules for scaling up |
| `scaleDown` | ScalingRules | Rules for scaling down |
| `scaleUp/scaleDown.stabilizationWindowSeconds` | int | Seconds to look back for flapping prevention |
| `scaleUp/scaleDown.selectPolicy` | string | `Min`, `Max`, or `Disabled` |
| `scaleUp/scaleDown.policies[]` | list | Scaling policies |
| `scaleUp/scaleDown.policies[].type` | string | `Pods` or `Percent` |
| `scaleUp/scaleDown.policies[].value` | int | Number of pods or percentage |
| `scaleUp/scaleDown.policies[].periodSeconds` | int | Time window for the policy |

## Local Development

Requires [k3d](https://k3d.io/).

```bash
make localdev          # create k3d cluster + install controller
make run               # run controller in dev pod
```

### Example

A dummy-metrics service and EPA resource are provided for testing. In a separate terminal from the running controller:

```bash
make localdev-examples
```

This deploys a `dummy-metrics` pod that exposes a configurable `queue_depth` metric on `/metrics`, and an `ExternalPodAutoscaler` that scrapes it and scales the dummy-metrics deployment between 1 and 10 replicas.

The metric value is driven by a ConfigMap. To simulate load and trigger scaling:

```bash
kubectl patch configmap dummy-metrics-value -n epa-system -p '{"data":{"value":"50"}}'
```

Set the value back to `"0"` to scale down.

## Building

```bash
make build       # cargo build --release
make test        # cargo test
make clippy      # cargo clippy
make fmt-check   # cargo fmt --check
```

## License

See [LICENSE](LICENSE) file.
