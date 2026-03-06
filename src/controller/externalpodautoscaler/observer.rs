use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use anyhow::Result;
use kube::{Api, Client};
use std::sync::Arc;
use std::time::SystemTime;

/// ObservedState contains all the observed resources for an EPA reconciliation
#[derive(Debug, Clone)]
pub struct ObservedState {
    /// The EPA resource being reconciled
    pub epa: Option<Arc<ExternalPodAutoscaler>>,
    /// Time when the observation was made
    pub observe_time: SystemTime,
}

impl ObservedState {
    /// Create a new empty ObservedState
    pub fn new() -> Self {
        Self {
            epa: None,
            observe_time: SystemTime::now(),
        }
    }

    /// Check if the EPA resource exists
    pub fn epa_exists(&self) -> bool {
        self.epa.is_some()
    }

    /// Get the EPA resource if it exists
    pub fn epa(&self) -> Option<&Arc<ExternalPodAutoscaler>> {
        self.epa.as_ref()
    }

    /// Check if the EPA is being deleted
    pub fn is_deleting(&self) -> bool {
        self.epa
            .as_ref()
            .map(|e| e.metadata.deletion_timestamp.is_some())
            .unwrap_or(false)
    }
}

impl Default for ObservedState {
    fn default() -> Self {
        Self::new()
    }
}

/// StateObserver handles observing and gathering the current state
pub struct StateObserver {
    client: Client,
    namespace: String,
    name: String,
}

impl StateObserver {
    /// Create a new StateObserver
    pub fn new(client: Client, namespace: String, name: String) -> Self {
        Self {
            client,
            namespace,
            name,
        }
    }

    /// Observe the current state and populate the ObservedState
    pub async fn observe(&self, observed: &mut ObservedState) -> Result<()> {
        // Observe the EPA resource
        let epa = self.observe_epa().await?;

        let epa = match epa {
            Some(epa) => Arc::new(epa),
            None => {
                // EPA doesn't exist (was deleted)
                observed.epa = None;
                return Ok(());
            }
        };

        observed.epa = Some(epa.clone());

        // Update observation time
        observed.observe_time = SystemTime::now();

        Ok(())
    }

    /// Observe the EPA resource
    async fn observe_epa(&self) -> Result<Option<ExternalPodAutoscaler>> {
        let api: Api<ExternalPodAutoscaler> = Api::namespaced(self.client.clone(), &self.namespace);

        match api.get(&self.name).await {
            Ok(epa) => Ok(Some(epa)),
            Err(kube::Error::Api(err)) if err.code == 404 => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
