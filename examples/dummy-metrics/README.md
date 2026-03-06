# dummy-metrics

Serves a single Prometheus gauge metric on `/metrics`, reading the value from
`/etc/dummy-metrics/value` on each request. All pods mount the same ConfigMap,
so updating the ConfigMap changes the value across all replicas.

## Usage

```sh
dummy-metrics <port> <metric_name> <service_dns>
```

## Update the metric value

```sh
kubectl patch configmap dummy-metrics-value -n epa-system -p '{"data":{"value":"42"}}'
```

## Verify

```sh
kubectl exec -n epa-system deploy/dummy-metrics -- curl -s localhost:9090/metrics
```
