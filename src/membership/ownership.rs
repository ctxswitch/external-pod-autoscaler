use crate::membership::manager::MembershipManager;
use fnv::FnvHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::Instant;

/// Buffer added to the evaluation period when determining how long the old
/// owner continues to scrape and serve after losing hash ownership.
pub(crate) const GRACE_BUFFER: Duration = Duration::from_secs(5);

/// Tracks when this replica became the hash winner for an EPA.
struct OwnershipState {
    /// The instant this replica became the hash winner.
    since: Instant,
    /// The EPA's configured evaluation period.
    evaluation_period: Duration,
}

/// Tracks when this replica lost hash ownership of an EPA.
struct LostOwnership {
    /// The instant ownership was lost.
    at: Instant,
    /// The EPA's configured evaluation period frozen at the instant this replica
    /// lost hash ownership. Not updated on subsequent ticks so that the grace
    /// window is bounded to the period known at loss time.
    evaluation_period: Duration,
}

/// Holds both ownership maps under a single lock to ensure atomic reads
/// and writes across ownership transitions.
struct TransitionState {
    owned: HashMap<String, OwnershipState>,
    lost: HashMap<String, LostOwnership>,
}

/// Assigns EPA ownership using rendezvous hashing (Highest Random Weight)
///
/// This provides consistent hashing with minimal rebalancing when replicas change.
/// Each EPA is assigned to the replica with the highest hash(replica_id + epa_key).
///
/// Ownership transitions are tracked so that the old owner continues to scrape
/// and serve metrics during a grace period, giving the new owner time to build
/// up a full evaluation window of data before taking over.
pub struct EpaOwnership {
    membership: Arc<MembershipManager>,
    state: RwLock<TransitionState>,
}

fn epa_key(namespace: &str, name: &str) -> String {
    format!("{}/{}", namespace, name)
}

impl EpaOwnership {
    /// Creates a new `EpaOwnership` backed by the given membership manager.
    pub fn new(membership: Arc<MembershipManager>) -> Self {
        Self {
            membership,
            state: RwLock::new(TransitionState {
                owned: HashMap::new(),
                lost: HashMap::new(),
            }),
        }
    }

    /// Returns the replica ID that currently owns this EPA via rendezvous hashing.
    ///
    /// All active replicas will agree on the same owner for a given `(namespace, name)` pair.
    /// Returns `None` if no replicas are active.
    ///
    /// # Arguments
    /// * `namespace` - EPA namespace
    /// * `name` - EPA name
    pub async fn get_epa_owner(&self, namespace: &str, name: &str) -> Option<String> {
        let key = epa_key(namespace, name);
        let active_replicas = self.membership.get_active_replicas().await;

        if active_replicas.is_empty() {
            return None;
        }

        Self::highest_weight_replica(active_replicas, &key)
    }

    /// Refresh ownership state for the given EPA.
    ///
    /// Must be called once per tick before `should_scrape_epa` or
    /// `should_serve_epa`. `should_scrape_epa` derives its answer entirely
    /// from the pre-computed `owned` and `lost` maps. `should_serve_epa`
    /// additionally queries live membership via [`get_previous_epa_owner`]
    /// when the evaluation period has not yet elapsed.
    ///
    /// # Concurrency
    /// Must not be called concurrently for the same `(namespace, name)` pair.
    /// Concurrent calls for different EPAs are safe. The calling loop must
    /// process EPAs sequentially (not via `FuturesUnordered` or `join!`).
    ///
    /// # Arguments
    /// * `namespace` - EPA namespace
    /// * `name` - EPA name
    /// * `evaluation_period` - The EPA's configured evaluation period
    pub async fn refresh_ownership(
        &self,
        namespace: &str,
        name: &str,
        evaluation_period: Duration,
    ) {
        let key = epa_key(namespace, name);
        let my_id = self.membership.my_replica_id();

        // Membership snapshot taken before acquiring the state lock. Staleness
        // is bounded by the lease renewal interval (10s) and corrected on the
        // next tick.
        let winner = self.get_epa_owner(namespace, name).await;
        let i_am_winner = winner.as_deref() == Some(my_id);

        let mut state = self.state.write().await;

        if i_am_winner {
            state
                .owned
                .entry(key.clone())
                .and_modify(|s| {
                    s.evaluation_period = evaluation_period;
                })
                .or_insert_with(|| OwnershipState {
                    since: Instant::now(),
                    evaluation_period,
                });
            state.lost.remove(&key);
        } else if state.owned.remove(&key).is_some() {
            // Preserve the loss timestamp across consecutive non-winning ticks.
            // A full re-win followed by re-loss resets the clock because
            // lost.remove is called on every winning tick.
            state.lost.entry(key).or_insert_with(|| LostOwnership {
                at: Instant::now(),
                evaluation_period,
            });
        }

        state
            .lost
            .retain(|_, entry| entry.at.elapsed() < entry.evaluation_period + GRACE_BUFFER);
    }

    /// Returns true if this replica should scrape metrics for the given EPA.
    ///
    /// Derives the answer from the pre-computed `owned` and `lost` maps.
    /// `refresh_ownership` must be called before this method on each tick.
    ///
    /// # Arguments
    /// * `namespace` - EPA namespace
    /// * `name` - EPA name
    pub async fn should_scrape_epa(&self, namespace: &str, name: &str) -> bool {
        let key = epa_key(namespace, name);
        let state = self.state.read().await;

        state.owned.contains_key(&key) || state.lost.contains_key(&key)
    }

    /// Returns true if this replica should serve scaling decisions for the
    /// given EPA.
    ///
    /// Derives the answer from the pre-computed `owned` and `lost` maps.
    /// `refresh_ownership` must be called before this method on each tick.
    ///
    /// When this replica is the current hash owner but the evaluation period
    /// has not yet elapsed, the method checks whether a previous owner (a
    /// draining replica) is still visible via [`get_previous_epa_owner`].
    /// If no previous owner is alive, serving begins immediately — partial
    /// data is better than no data. If a previous owner is still draining,
    /// returns `false` until the evaluation period elapses.
    ///
    /// # Arguments
    /// * `namespace` - EPA namespace
    /// * `name` - EPA name
    pub async fn should_serve_epa(&self, namespace: &str, name: &str) -> bool {
        let key = epa_key(namespace, name);

        {
            let state = self.state.read().await;

            if let Some(s) = state.owned.get(&key) {
                if s.since.elapsed() >= s.evaluation_period {
                    return true;
                }
            } else {
                return state.lost.contains_key(&key);
            }
        }

        let has_previous_owner = match self.get_previous_epa_owner(namespace, name).await {
            Some(prev) => prev != self.membership.my_replica_id(),
            None => false,
        };

        // Partial data is better than no data — do not wait for a predecessor that no longer exists.
        !has_previous_owner
    }

    /// Returns the replica ID that would own this EPA if draining replicas were
    /// still active members.
    ///
    /// Runs rendezvous hashing over the union of active and draining replicas.
    /// This identifies the replica that held ownership before the drain transition
    /// began. When the draining replica was the hash winner, this returns that
    /// draining replica rather than its active successor. When no replicas are
    /// draining, or when the draining replica was not the hash winner, this
    /// returns the same result as [`get_epa_owner`].
    ///
    /// # Arguments
    /// * `namespace` - EPA namespace
    /// * `name` - EPA name
    ///
    /// # Returns
    /// The replica ID of the highest-weight hash winner across the combined
    /// active+draining set, or `None` if both sets are empty.
    pub(crate) async fn get_previous_epa_owner(
        &self,
        namespace: &str,
        name: &str,
    ) -> Option<String> {
        let key = epa_key(namespace, name);
        let all_replicas = self.membership.get_all_replicas().await;

        if all_replicas.is_empty() {
            return None;
        }

        Self::highest_weight_replica(all_replicas, &key)
    }

    /// Returns the replica with the highest rendezvous hash weight for the
    /// given EPA key, or `None` if `replicas` is empty.
    ///
    /// # Precondition
    /// `replicas` must contain no duplicate IDs. A duplicated ID is counted
    /// twice by `max_by_key`, skewing the election toward that replica.
    fn highest_weight_replica(
        replicas: impl IntoIterator<Item = String>,
        key: &str,
    ) -> Option<String> {
        replicas
            .into_iter()
            .map(|replica_id| {
                let weight = Self::compute_weight(&replica_id, key);
                (replica_id, weight)
            })
            .max_by_key(|(_, weight)| *weight)
            .map(|(replica_id, _)| replica_id)
    }

    /// Computes the rendezvous hash weight for a replica + EPA key pair.
    ///
    /// Uses FNV-1a (via `fnv::FnvHasher`) because its output is stable across
    /// Rust versions and process restarts. Do not substitute `DefaultHasher` —
    /// it is explicitly version-unstable, which would cause different replicas
    /// to disagree on ownership.
    fn compute_weight(replica_id: &str, epa_key: &str) -> u64 {
        let mut hasher = FnvHasher::default();
        replica_id.hash(&mut hasher);
        epa_key.hash(&mut hasher);
        hasher.finish()
    }
}
