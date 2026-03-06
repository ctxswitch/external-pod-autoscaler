use dashmap::DashMap;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::runtime::{reflector, watcher, WatchStreamExt};
use kube::Api;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tracing::{error, info, warn};

/// Shared cache of pod reflector stores, keyed by namespace.
///
/// One reflector watches all pods in a namespace. Multiple workers serving
/// different EPAs in the same namespace share a single reflector, avoiding
/// redundant Kubernetes API server watches.
///
/// Pod discovery is zero-copy once the reflector is running: workers read
/// from the in-memory store and filter by label selector without making any
/// API calls.
///
/// **Scalability note:** Each reflector watches _all_ pods in its namespace,
/// not just those matching a specific label selector. This keeps the design
/// simple (one reflector per namespace regardless of how many EPAs target
/// different workloads in that namespace) but means memory scales with total
/// pod count per namespace. For very large namespaces (>1000 pods), consider
/// adding label-filtered watchers.
pub struct PodCache {
    /// namespace → reflector store (only inserted after initial sync)
    stores: Arc<DashMap<String, reflector::Store<Pod>>>,
    /// Tracks namespaces that have a reflector being started, to prevent
    /// duplicate reflectors when multiple workers call ensure_namespace
    /// concurrently for the same namespace.
    starting: Arc<Mutex<HashSet<String>>>,
    /// Notifies waiting callers when a namespace store is published.
    notify: Arc<Notify>,
    client: kube::Client,
}

impl PodCache {
    /// Creates a new, empty `PodCache`.
    pub fn new(client: kube::Client) -> Self {
        Self {
            stores: Arc::new(DashMap::new()),
            starting: Arc::new(Mutex::new(HashSet::new())),
            notify: Arc::new(Notify::new()),
            client,
        }
    }

    /// Ensures a reflector is running for `namespace`, starting one if needed.
    ///
    /// On the first call for a given namespace this starts the reflector **and
    /// waits for the initial list to populate the store** so that the caller
    /// immediately sees the current set of pods. Subsequent calls are a no-op.
    ///
    /// The wait is bounded by a 30-second timeout so that a temporary API
    /// server outage does not permanently stall the worker's scrape loop.
    pub async fn ensure_namespace(&self, namespace: &str) {
        // Fast path: reflector already running and synced.
        if self.stores.contains_key(namespace) {
            return;
        }

        // Serialize concurrent callers for the same namespace. Only the first
        // caller starts the reflector; others wait for a notification.
        {
            let mut starting = self.starting.lock().await;
            if self.stores.contains_key(namespace) {
                return;
            }
            if !starting.insert(namespace.to_string()) {
                // Another task is already starting this namespace's reflector.
                // Loop on notifications until our namespace appears or we time out.
                // This handles spurious wakeups from other namespaces' notify_waiters.
                let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                drop(starting);
                loop {
                    if self.stores.contains_key(namespace) {
                        return;
                    }
                    let notified = self.notify.notified();
                    if tokio::time::timeout_at(deadline, notified).await.is_err() {
                        warn!(
                            namespace,
                            "Timed out waiting for another task to start pod reflector"
                        );
                        return;
                    }
                }
            }
        }

        let (reader, writer) = reflector::store::<Pod>();
        let reader_clone = reader.clone();

        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), namespace);
        let namespace_owned = namespace.to_string();
        let ns_for_task = namespace_owned.clone();

        // Clone handles so the background task can clean up on exit.
        let stores_handle = self.stores.clone();
        let starting_handle = self.starting.clone();
        let notify_handle = self.notify.clone();

        let task_handle = tokio::spawn(async move {
            info!(namespace = %namespace_owned, "Starting pod reflector");

            let stream = watcher(pod_api, watcher::Config::default())
                .default_backoff()
                .reflect(writer)
                .applied_objects();

            tokio::pin!(stream);

            loop {
                match stream.next().await {
                    Some(Ok(_)) => {} // Pod event processed; store is updated by reflect().
                    Some(Err(e)) => {
                        warn!(
                            namespace = %namespace_owned,
                            error = %e,
                            "Pod reflector encountered an error; will retry"
                        );
                    }
                    None => {
                        error!(
                            namespace = %namespace_owned,
                            "Pod reflector stream ended unexpectedly; evicting store"
                        );
                        // Evict the store and clear the starting flag so the next
                        // ensure_namespace call can start a fresh reflector.
                        stores_handle.remove(&ns_for_task);
                        starting_handle.lock().await.remove(&ns_for_task);
                        notify_handle.notify_waiters();
                        break;
                    }
                }
            }
        });

        // Wait for the initial list to populate the store, with a timeout so
        // that a temporarily unreachable API server doesn't stall the worker.
        let ready =
            tokio::time::timeout(Duration::from_secs(30), reader_clone.wait_until_ready()).await;

        let synced = matches!(ready, Ok(Ok(())));

        if synced {
            info!(namespace, "Pod reflector initial sync completed");
            // Only publish the store after a successful initial sync.
            self.stores.insert(namespace.to_string(), reader);
        } else {
            // Abort the orphaned reflector task to avoid leaking background work.
            task_handle.abort();

            match ready {
                Ok(Err(_)) => {
                    warn!(
                        namespace,
                        "Pod reflector writer dropped before initial sync; will retry next scrape"
                    );
                }
                Err(_) => {
                    warn!(
                        namespace,
                        "Pod reflector initial sync timed out after 30s; will retry next scrape"
                    );
                }
                Ok(Ok(())) => {
                    // Logically unreachable: synced would be true. Log and
                    // continue defensively rather than panicking.
                    warn!(
                        namespace,
                        "Unexpected state: ready succeeded but sync flag was false"
                    );
                }
            }
        }

        // Remove from the starting set so a retry can be attempted, and
        // notify any concurrent callers waiting for this namespace.
        self.starting.lock().await.remove(namespace);
        self.notify.notify_waiters();
    }

    /// Returns ready pods in `namespace` that match `label_selector`.
    ///
    /// `label_selector` is the standard Kubernetes comma-separated equality
    /// format produced by [`Worker::build_label_selector_string`], e.g.
    /// `"app=worker,tier=backend"`.
    ///
    /// A pod is considered ready when:
    /// - Phase is `Running`
    /// - It has a pod IP
    /// - Its `Ready` condition is `True`
    /// - `deletionTimestamp` is not set (pod is not terminating)
    ///
    /// On the first call for a namespace, the reflector is lazily started and
    /// its initial list is awaited so that the returned set is immediately
    /// populated.
    ///
    /// # Errors
    ///
    /// Returns an error if the reflector for the namespace failed to start
    /// (e.g., API server unreachable, RBAC misconfiguration). This lets the
    /// caller distinguish "no matching pods" from "unable to list pods".
    pub async fn get_ready_pods(
        &self,
        namespace: &str,
        label_selector: &str,
    ) -> anyhow::Result<Vec<Arc<Pod>>> {
        self.ensure_namespace(namespace).await;

        let store = self.stores.get(namespace).ok_or_else(|| {
            anyhow::anyhow!(
                "Pod reflector for namespace '{}' is not ready; will retry next scrape",
                namespace
            )
        })?;

        let selector_pairs = parse_label_selector(label_selector);

        Ok(store
            .state()
            .into_iter()
            .filter(|pod| is_pod_ready(pod) && pod_matches_selector(pod, &selector_pairs))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parses `"key=value,key2=value2"` into a `Vec<(String, String)>`.
///
/// Entries that do not contain `=` are silently ignored so that an empty or
/// malformed selector does not crash the worker.
pub(crate) fn parse_label_selector(selector: &str) -> Vec<(String, String)> {
    if selector.is_empty() {
        return Vec::new();
    }

    selector
        .split(',')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?.trim();
            let value = parts.next()?.trim();
            if key.is_empty() {
                None
            } else {
                Some((key.to_string(), value.to_string()))
            }
        })
        .collect()
}

/// Returns `true` when the pod is in the `Running` phase, has a pod IP,
/// its `Ready` condition is `True`, and `deletionTimestamp` is not set
/// (i.e., the pod is not terminating).
pub(crate) fn is_pod_ready(pod: &Pod) -> bool {
    // Exclude terminating pods (deletionTimestamp is set)
    if pod.metadata.deletion_timestamp.is_some() {
        return false;
    }

    let status = match pod.status.as_ref() {
        Some(s) => s,
        None => return false,
    };

    if !status
        .phase
        .as_deref()
        .map(|p| p == "Running")
        .unwrap_or(false)
    {
        return false;
    }

    if status.pod_ip.is_none() {
        return false;
    }

    status
        .conditions
        .as_ref()
        .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"))
        .map(|c| c.status == "True")
        .unwrap_or(false)
}

/// Returns `true` when the pod's labels contain every `(key, value)` pair in
/// `selector_pairs`.
pub(crate) fn pod_matches_selector(pod: &Pod, selector_pairs: &[(String, String)]) -> bool {
    if selector_pairs.is_empty() {
        return true;
    }

    let pod_labels = match pod.metadata.labels.as_ref() {
        Some(l) => l,
        None => return false,
    };

    selector_pairs
        .iter()
        .all(|(k, v)| pod_labels.get(k).map(|pv| pv == v).unwrap_or(false))
}
