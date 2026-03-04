# Lease-Based Membership and Work Assignment

The External Pod Autoscaler (EPA) runs as a multi-replica deployment. Replicas coordinate through Kubernetes Lease objects to discover each other, divide scraping work, and forward webhook requests to the correct owner. No external coordination service is required.

## Overview

```
                       Kubernetes API
                            |
              +-------------+-------------+
              |             |             |
         Replica A     Replica B     Replica C
         (Lease A)     (Lease B)     (Lease C)
              |             |             |
         Watches all   Watches all   Watches all
          leases        leases        leases
              |             |             |
         Computes      Computes      Computes
         ownership     ownership     ownership
         (same result) (same result) (same result)
```

Each replica independently arrives at the same ownership decisions through deterministic hashing. There is no leader, no election, and no consensus protocol.

## Replica Lifecycle

### Registration

On startup, each replica reads its identity from the Downward API environment variables:

| Variable | Purpose |
|----------|---------|
| `POD_UID` | Unique replica ID |
| `POD_IP` | Network address for request forwarding |
| `POD_NAME` | Pod name for logging |
| `POD_NAMESPACE` | Namespace for lease placement |

The replica creates a Lease named `epa-replica-{POD_UID}` in its namespace:

```yaml
apiVersion: coordination.k8s.io/v1
kind: Lease
metadata:
  name: epa-replica-<pod-uid>
  namespace: <pod-namespace>
  labels:
    app: external-pod-autoscaler
    replica-id: <pod-uid>
  ownerReferences:
    - apiVersion: v1
      kind: Pod
      name: <pod-name>
      uid: <pod-uid>
      # controller and blockOwnerDeletion are not set
spec:
  holderIdentity: "<pod-ip>:<webhook-port>"
  leaseDurationSeconds: 30
  renewTime: "<now>"
  acquireTime: "<now>"
```

The `ownerReferences` entry ensures the lease is garbage-collected by the Kubernetes GC controller after the Pod object is deleted. This prevents orphaned leases from accumulating after abnormal terminations where the process cannot clean up its own lease.

`holderIdentity` stores the `IP:port` pair used for webhook request forwarding.

### Renewal

A renewal loop runs every 10 seconds. Each tick patches the lease to update `renewTime`. The 30-second lease duration gives a 3x safety margin over the renewal interval, so a replica can miss up to two renewal cycles before being considered expired.

If the lease does not yet exist (first registration), it is created. If it already exists (409 conflict), it is patched via server-side apply.

### Discovery

A separate loop watches all leases labeled `app=external-pod-autoscaler` in the namespace. On each watch event, the replica refreshes its in-memory view of active replicas by listing all leases and filtering:

```
active = leases where (now < renewTime + leaseDurationSeconds)
```

Expired leases are excluded. Valid leases are partitioned into two sets based on the `draining` label:

- **Active**: leases without `draining=true` — included in rendezvous hash calculations.
- **Draining**: leases with `draining=true` — excluded from hash calculations but their addresses remain discoverable for request forwarding.

The replica ID is extracted from the lease name by stripping the `epa-replica-` prefix. Both sets are updated atomically under a single `RwLock`.

### Expiry

When a replica crashes or is terminated, it stops renewing its lease. Within 30 seconds, all other replicas observe the expiry during their next `update_active_replicas` call. The expired replica's EPAs are automatically reassigned through the hashing algorithm on the next worker tick.

## Work Assignment

### Rendezvous Hashing

EPA-to-replica assignment uses rendezvous hashing (Highest Random Weight). For each EPA, every active replica computes a deterministic weight:

```
weight(replica_id, epa_key) = hash(replica_id + epa_namespace/epa_name)
```

The replica with the highest weight owns the EPA. This algorithm has two important properties:

1. **Deterministic**: All replicas independently compute the same owner for any given EPA, without communication.
2. **Minimal disruption**: When a replica joins or leaves, only the EPAs that were assigned to the changed replica are redistributed. All other assignments remain stable.

### Assignment

- `EpaOwnership.should_scrape_epa(namespace, name)` returns `true` if this replica should collect metrics for the EPA — either as the current hash winner or during a grace period after losing ownership.
- `EpaOwnership.should_serve_epa(namespace, name)` returns `true` if this replica should respond to webhook queries. Returns `true` after collecting a full evaluation window of data, during the grace period after losing ownership, or immediately if the new owner detects that no previous owner is still alive (partial data is better than no data).

Both checks happen on every scheduler tick (1 second). No explicit handoff or rebalancing protocol is needed.

## Scraper Architecture

The scraper uses a scheduler/worker-pool design connected by a shared MPMC channel (`async-channel`). This distributes work at the pod level rather than the EPA level, so an EPA with many pods is scraped by all workers in parallel rather than bottlenecking on a single worker.

### Scheduler

A single scheduler task ticks every second and produces per-pod scrape jobs:

```
loop {
    for each active EPA:
        epa_ownership.refresh_ownership(namespace, name, evaluation_period)
        if !epa_ownership.should_scrape_epa(namespace, name):
            skip (not this replica's assignment)
        if time_since_last_scrape < scrape_interval AND interval unchanged:
            skip (not time yet)
        resolve target pods via PodCache
        for each ready pod:
            send ScrapeJob to worker pool via channel
    sleep until next 1-second tick
}
```

The scheduler owns all EPA-level logic: ownership checks, interval timing, pod resolution, and metric configuration. It tracks the interval alongside each deadline so that when an EPA's scrape interval changes, the stale deadline is invalidated and the EPA is re-scraped immediately. It reads from a shared `active_epas` map that the controller updates via a channel. When the controller reconciles an EPA, it sends an `EpaUpdate::Upsert` or `EpaUpdate::Delete` message. The scheduler picks up changes on its next tick without restart.

### Workers

Workers are simple channel consumers. Each worker loops on the shared receiver, scrapes the target pod, stores samples, and records telemetry. Workers have no awareness of EPA ownership or scheduling — they process whatever job arrives.

```
loop {
    job = receive from channel
    scrape job.pod using job.epa scrape config
    store samples in metrics_store
}
```

The worker pool size is configurable via `SCRAPER_WORKERS` (default: 20). The channel is bounded at `worker_count * 64` slots, providing backpressure when workers cannot keep up. Queue wait time is measured by the `epa_scrape_enqueue_wait_seconds` histogram.

### Pod Discovery

Pod discovery uses per-namespace reflectors (`PodCache`) that maintain an in-memory store of pods matching the EPA's label selector. The first lookup for a namespace starts a reflector and waits for initial sync. Subsequent lookups are zero-API-call reads from the in-memory store.

## Webhook Request Forwarding

The webhook server serves the external metrics API that the HPA queries. When a metrics request arrives at any replica:

1. **Parse the metric name** to extract the EPA identity. External metric names follow the format `{epa-name}-{epa-namespace}-{metric-name}`.

2. **Check serving eligibility** via `epa_ownership.should_serve_epa()`. This returns `true` if this replica has been scraping long enough to have a full evaluation window, if it recently lost ownership and is still in the grace period, or if no previous owner is still alive.

3. **If this replica should serve**: aggregate samples from the local `metrics_store` and return the result. Results are cached for 10 seconds.

4. **If this replica should not serve**: determine a forward target:
   - Get the hash winner via `get_epa_owner` (active replicas only).
   - If the winner is a different replica, forward to it.
   - If the winner is this replica (new owner, evaluation window not yet full), call `get_previous_epa_owner` which runs the hash over the union of active and draining replicas. If the previous owner is a different replica (the draining predecessor), forward to it.
   - If no forward target is found, or the forward fails, fall back to local data if available. If no local data exists, return empty metrics.

```
HPA → APIService → Any Replica's Webhook Server
                         |
                    Should this replica
                    serve this EPA?
                    /           \
                  Yes            No
                  |               |
            Aggregate from    Am I the hash winner?
            local metrics     /                  \
            store           No                   Yes (but can't serve yet)
                            |                      |
                     Forward to hash winner    Forward to previous owner
                     via lease address         (draining replica)
                            |                      |
                       Forward failed?        Forward failed?
                       /           \          /           \
                     No             Yes     No             Yes
                     |               |      |               |
                Return result    Serve from local data if available
```

The maximum hop count is two: a non-owner forwards to the active hash winner, which may itself forward once to the draining predecessor. Cycles are impossible because the `prev != owner_id` guard prevents self-forwarding when no draining replica exists.

The forward client uses TLS with certificate verification disabled (pods use self-signed certificates) and has a 2-second connect timeout, 5-second request timeout, and pools up to 10 idle connections per host.

## Graceful Ownership Handoff

When ownership of an EPA moves between replicas, the new owner starts with an empty metrics store. Serving metrics immediately would mean the HPA receives values based on a partial evaluation window, potentially causing erratic scaling. To prevent this, ownership transitions use a two-phase handoff.

### Scraping vs Serving

Work assignment separates two concerns:

- **Scraping** (`should_scrape_epa`): Should this replica collect metrics? Returns `true` immediately when the hash assigns the EPA to this replica, and continues returning `true` during the grace period after losing ownership.
- **Serving** (`should_serve_epa`): Should this replica respond to webhook queries? Returns `true` after the replica has been scraping long enough to fill the evaluation window (`evaluation_period`), during the grace period after losing ownership, or immediately if no previous owner is still alive (e.g., the previous owner crashed and its lease expired).

### Transition Timeline

When the previous owner is still alive (graceful drain or scale-down):

```
                  hash changes               evaluation_period elapses
                       |                             |
  Old owner:  [scraping + serving]--------[scraping + serving]---[stops]
  New owner:       [scraping only]--------[scraping only]----[scraping + serving]
```

When the previous owner is gone (crash, lease expired):

```
                  hash changes
                       |
  Old owner:  [gone — lease expired]
  New owner:       [scraping + serving immediately]
```

The old owner continues scraping and serving for `evaluation_period + GRACE_BUFFER` (default 5 seconds) after losing the hash. The new owner begins scraping immediately. If a previous owner is still alive (visible in the active or draining replica set), the new owner waits for `evaluation_period` before serving. If no previous owner exists — because the previous owner crashed and its lease expired — the new owner serves immediately with whatever data it has, since partial data is better than no data.

The overlap is guaranteed structurally, not by synchronized clocks:

1. The rendezvous hash result is deterministic — both replicas agree on who won.
2. Both observe the same membership change via the lease watcher, though not necessarily at the same instant.
3. `GRACE_BUFFER` (5s) is sized to absorb propagation jitter between the two replicas' observation times and 1-second tick alignment.

### Ownership State Machine

Each replica tracks ownership state per EPA via `refresh_ownership()`, called by the scheduler once per tick:

| State | `should_scrape_epa` | `should_serve_epa` |
|-------|--------------------|--------------------|
| **Owned** (hash winner, `elapsed < evaluation_period`, previous owner alive) | `true` | `false` |
| **Owned** (hash winner, `elapsed < evaluation_period`, no previous owner) | `true` | `true` |
| **Owned** (hash winner, `elapsed >= evaluation_period`) | `true` | `true` |
| **Lost** (within `evaluation_period + GRACE_BUFFER`) | `true` | `true` |
| **Not tracking** (never owned, or grace window elapsed) | `false` | `false` |

## Graceful Shutdown

When a replica receives SIGTERM (scale-down, rolling update), it initiates a drain sequence rather than exiting immediately. This ensures the HPA continues receiving metrics based on a full evaluation window throughout the transition.

### Drain Sequence

```
SIGTERM received         evaluation_period elapses        drain complete
      |                          |                             |
      v                          v                             v
 mark lease              new owner starts               delete lease
 draining                serving                        + exit
      |                          |                             |
 Old pod:  [scraping + serving]--[scraping + serving]-------[stops]
 New owner:     [scraping only]--[scraping only]--[scraping + serving]
```

1. **Mark draining**: The replica adds a `draining=true` label to its lease via `mark_draining()`. The renewal loop continues — the lease stays alive and the replica's `holderIdentity` remains discoverable for request forwarding. Draining replicas are excluded from the active set used by rendezvous hashing, so EPAs are immediately reassigned to surviving active replicas.

2. **Drain sleep**: The replica sleeps for `DRAIN_DURATION` (configurable via environment variable, default 120 seconds). During this period all components keep running: the renewal loop renews the draining lease, the scheduler and worker pool continue collecting metrics for EPAs in the `lost` grace window, and the webhook server continues responding to forwarded requests.

3. **Delete lease**: After the drain period, the renewal loop task is cancelled and awaited. `delete_lease()` then deletes the lease from Kubernetes and sets the local draining flag as a guard, preventing any in-flight renewal from re-creating the lease as active. The process then exits.

A second SIGTERM or SIGINT during the drain sleep interrupts the sleep and proceeds to task cancellation, lease deletion, and exit — the same steps that occur at the end of a completed drain.

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `DRAIN_DURATION` | `120` | Seconds to keep serving after SIGTERM before exiting |

The pod's `terminationGracePeriodSeconds` must be at least `DRAIN_DURATION + 30` seconds to allow the full drain sequence plus lease deletion. If the kubelet kills the process before the drain completes, the lease's `ownerReferences` ensure it is garbage-collected after the Pod object is deleted.

### SIGINT Behavior

SIGINT (Ctrl-C) skips both the draining label and the drain sleep for development convenience. The lease is deleted immediately without first being marked draining, so peers do not wait for the 30-second TTL to expire. Because the draining label is never set, EPAs are not proactively reassigned before exit; peers discover the change only after the lease disappears.

## Dynamic Rebalancing

When the replica count changes (scale up, scale down, crash, rollout):

1. **Scale up / new replica**: The new replica creates a lease. All replicas discover it via the lease watcher. Rendezvous hashing reassigns some EPAs to the new replica. The new owner begins scraping immediately. The old owner continues scraping and serving during the grace period. After `evaluation_period` elapses, the new owner starts serving. The system converges within one renewal cycle (~10 seconds).

2. **Graceful scale down / rollout**: The departing replica receives SIGTERM, marks its lease as draining, and enters the drain sequence (see Graceful Shutdown above). Draining replicas are excluded from the hash ring, so EPAs are reassigned immediately. The draining replica continues scraping and serving via the `lost` grace window. Webhook requests to the new owner (which cannot serve yet) are forwarded to the draining replica via the address stored in the draining lease. Convergence occurs within `evaluation_period`.

3. **Crash / ungraceful termination**: The crashed replica's lease expires within 30 seconds (no drain sequence). EPAs are reassigned to remaining replicas. Once the crashed replica's lease expires and is no longer visible in the active or draining sets, new owners serve immediately with whatever data they have — partial data is better than no data. The `ownerReferences` on the lease ensure garbage collection after the Pod object is deleted.

In all cases, only EPAs previously assigned to the changed replica are affected. All other assignments remain stable.

## Failure Modes

| Scenario | Behavior |
|----------|----------|
| Replica crashes | Lease expires in 30s; EPAs reassigned to remaining replicas; new owners serve immediately once the crashed replica's lease expires; ownerReferences GC cleans up lease after Pod deletion |
| Replica draining (SIGTERM) | Lease marked draining immediately; EPAs reassigned to active replicas; draining replica keeps serving during grace window; webhook requests forwarded to draining replica until new owner can serve |
| Drain interrupted (second signal) | Drain sleep aborted; lease deleted immediately; process exits |
| Kubelet kills during drain | Lease left in draining state; ownerReferences GC cleans up after Pod deletion; new owners may serve partial data during the gap |
| Renewal fails | Logged as warning; retried on next 10s tick; replica stays active until lease expires |
| Lease watcher disconnects | Reconnects with default backoff; active set is stale until reconnection |
| Owner unreachable during forward | Webhook falls back to local data if available; returns empty metrics if no local data exists |
| Split brain (network partition) | Each partition scrapes independently; metrics may be stale in the smaller partition |
