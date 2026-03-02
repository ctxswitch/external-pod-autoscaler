use crate::controller::membership::MembershipManager;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Assigns EPA ownership using rendezvous hashing (Highest Random Weight)
///
/// This provides consistent hashing with minimal rebalancing when replicas change.
/// Each EPA is assigned to the replica with the highest hash(replica_id + epa_key).
pub struct WorkAssigner {
    membership: Arc<MembershipManager>,
}

impl WorkAssigner {
    /// Create new work assigner
    pub fn new(membership: Arc<MembershipManager>) -> Self {
        Self { membership }
    }

    /// Check if this replica should handle the given EPA
    ///
    /// Uses rendezvous hashing to deterministically assign EPAs to replicas.
    /// All replicas will agree on ownership based on active replica set.
    ///
    /// # Arguments
    /// * `namespace` - EPA namespace
    /// * `name` - EPA name
    pub async fn should_handle_epa(&self, namespace: &str, name: &str) -> bool {
        let epa_key = format!("{}/{}", namespace, name);
        let my_id = self.membership.my_replica_id();

        match self.get_epa_owner(&epa_key).await {
            Some(owner_id) => owner_id == my_id,
            None => false, // No active replicas
        }
    }

    /// Get the replica ID that owns this EPA
    ///
    /// Returns None if no replicas are active.
    ///
    /// # Arguments
    /// * `epa_key` - EPA key in format "namespace/name"
    pub async fn get_epa_owner(&self, epa_key: &str) -> Option<String> {
        let active_replicas = self.membership.get_active_replicas().await;

        if active_replicas.is_empty() {
            return None;
        }

        // Rendezvous hashing: find replica with highest hash(replica_id + epa_key)
        active_replicas
            .into_iter()
            .map(|replica_id| {
                let weight = Self::compute_weight(&replica_id, epa_key);
                (replica_id, weight)
            })
            .max_by_key(|(_, weight)| *weight)
            .map(|(replica_id, _)| replica_id)
    }

    /// Compute rendezvous hash weight for replica + EPA combination
    pub(crate) fn compute_weight(replica_id: &str, epa_key: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        replica_id.hash(&mut hasher);
        epa_key.hash(&mut hasher);
        hasher.finish()
    }
}
