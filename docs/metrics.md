# Metrics Reference

The External Pod Autoscaler exposes Prometheus metrics from three subsystems: the controller, the scraper, and the webhook (external metrics API). All metrics use the `epa_` prefix.

## Controller Metrics

Defined in `src/controller/externalpodautoscaler/telemetry.rs`.

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `epa_reconcile_duration_seconds` | Histogram | `epa`, `namespace` | Time spent reconciling ExternalPodAutoscaler resources |
| `epa_reconcile_errors_total` | Counter | `epa`, `namespace`, `error_type` | Total number of reconciliation errors |
| `epa_hpa_operations_total` | Counter | `epa`, `namespace`, `operation` | Total number of HPA operations (create/update/delete) |

## Scraper Metrics

Defined in `src/scraper/telemetry.rs`.

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `epa_scrape_duration_seconds` | Histogram | `epa`, `namespace` | Time spent scraping metrics from pods |
| `epa_scrape_errors_total` | Counter | `epa`, `namespace`, `error_type` | Total number of scrape errors |
| `epa_pods_scraped_total` | Counter | `epa`, `namespace` | Total number of pods scraped per EPA |
| `epa_scrape_enqueue_wait_seconds` | Histogram | `epa`, `namespace` | Time blocked waiting to enqueue a scrape job (non-zero only under backpressure) |

Custom buckets for `epa_scrape_enqueue_wait_seconds`: `0.0001, 0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0`

## Webhook / External Metrics API Metrics

Defined in `src/webhook/metrics/telemetry.rs`.

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `epa_api_requests_total` | Counter | `namespace`, `metric`, `status` | Total number of API requests to the external metrics endpoint |
| `epa_api_request_duration_seconds` | Histogram | `namespace`, `metric` | Duration of API requests to the external metrics endpoint |
| `epa_cache_operations_total` | Counter | `namespace`, `metric`, `result` | Cache hits and misses for aggregated metrics |
