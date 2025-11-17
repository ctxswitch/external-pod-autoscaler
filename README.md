# External Metrics Collector

A Kubernetes operator for collecting and aggregating metrics from pods using Custom Resource Definitions (CRDs).

## Metric Custom Resource Definition

The Metric CRD allows you to define how to collect and aggregate metrics from pods in your Kubernetes cluster.

### Complete Example

```yaml
apiVersion: ctx.sh/v1beta1
kind: Metric
metadata:
  name: application-metrics
  namespace: default
spec:
  # Selector for finding pods to scrape
  selector:
    matchLabels:
      app: my-application
      environment: production
    matchExpressions:
      - key: tier
        operator: In
        values:
          - frontend
          - backend
  
  # Target configuration
  target:
    # Port to scrape metrics from (default: 8080)
    port: 9090
    
    # HTTP path to scrape metrics from (default: "/metrics")
    path: "/metrics"
    
    # Scrape interval in seconds (default: 30)
    intervalSeconds: 60
    
    # Scrape timeout in seconds (default: 10)
    timeoutSeconds: 15
    
    # Prometheus metric name to scrape (required)
    metricName: "http_requests_total"
    
    # Type of metric value: "gauge" or "counter" (default: "gauge")
    valueType: "counter"
    
    # Window duration for aggregation in seconds (default: 120)
    windowDurationSeconds: 300
    
    # Aggregation type: "avg", "sum", "min", "max", or "rate"
    # Default: "avg" for gauge, "rate" for counter
    aggregation: "rate"
```

## Field Definitions

### spec.selector

**Required.** Defines which pods to scrape metrics from using standard Kubernetes label selectors.

- **matchLabels** (map[string]string): Exact label matches. All specified labels must match (AND operation).

- **matchExpressions** (array): Advanced label matching expressions. Each expression contains:
  - **key** (string): The pod label key to match against
  - **operator** (string): Matching operator. Options:
    - `"In"`: Label value must be one of the specified values
    - `"NotIn"`: Label value must not be any of the specified values
    - `"Exists"`: Label with this key must exist (values ignored)
    - `"DoesNotExist"`: Label with this key must not exist
  - **values** (array[string]): Values for the operator (required for In/NotIn, ignored for Exists/DoesNotExist)

### spec.target

**Required.** Configuration for how to scrape metrics from selected pods.

- **port** (int32): TCP port to connect to for scraping metrics. Default: `8080`

- **path** (string): HTTP path to scrape metrics from. Default: `/metrics`

- **intervalSeconds** (int32): How often to scrape metrics in seconds. Default: `30`

- **timeoutSeconds** (int32): Maximum time to wait for a scrape response in seconds. Default: `10`

- **metricName** (string): **Required.** The Prometheus metric name to collect from the metrics endpoint.

- **valueType** (string): Type of metric being collected. Options:
  - `"gauge"`: For metrics that can go up and down (e.g., memory usage, queue size)
  - `"counter"`: For metrics that only increase (e.g., request count, error count)
  - Default: `"gauge"`

- **windowDurationSeconds** (int32): Time window for aggregation calculations in seconds. Default: `120`

- **aggregation** (string): How to aggregate metrics across multiple pods. Options:
  - `"avg"`: Average value across pods
  - `"sum"`: Sum of values across pods
  - `"min"`: Minimum value across pods
  - `"max"`: Maximum value across pods
  - `"rate"`: Rate of change (typically used with counters)
  - Default: `"avg"` for gauge metrics, `"rate"` for counter metrics


## Status Fields

The Metric resource status provides information about the current state of metric collection:

```yaml
status:
  # Total number of target pods
  totalTargets: 5
  
  # Number of successfully scraped pods
  scrapedTargets: 4
  
  # Last successful scrape timestamp
  lastScrapeTime: "2024-01-01T12:00:00Z"
```

## Usage Examples

### Basic Gauge Metric

```yaml
apiVersion: ctx.sh/v1beta1
kind: Metric
metadata:
  name: memory-usage
spec:
  selector:
    matchLabels:
      app: web-server
  target:
    metricName: "container_memory_usage_bytes"
    valueType: "gauge"
    aggregation: "avg"
```

### Counter Metric with Rate Calculation

```yaml
apiVersion: ctx.sh/v1beta1
kind: Metric
metadata:
  name: request-rate
spec:
  selector:
    matchLabels:
      app: api-server
  target:
    metricName: "http_requests_total"
    valueType: "counter"
    aggregation: "rate"
    windowDurationSeconds: 60
```

### Metric with Advanced Selector

```yaml
apiVersion: ctx.sh/v1beta1
kind: Metric
metadata:
  name: queue-depth
spec:
  selector:
    matchLabels:
      app: worker
    matchExpressions:
      - key: version
        operator: In
        values:
          - v2
          - v3
      - key: canary
        operator: DoesNotExist
  target:
    metricName: "job_queue_depth"
    valueType: "gauge"
    aggregation: "sum"
```
