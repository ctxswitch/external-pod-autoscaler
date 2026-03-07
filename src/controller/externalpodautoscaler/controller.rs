use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::externalpodautoscaler::reconcile::Reconciler;
use crate::membership::ownership::EpaOwnership;
use crate::scraper::EpaUpdate;
use crate::store::MetricsStore;
use futures::StreamExt;
use kube::runtime::{
    WatchStreamExt, controller::Controller as KubeController, predicates, reflector, watcher,
};
use kube::{Api, Client};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

/// Controller for ExternalPodAutoscaler resources.
///
/// Manages the kube-runtime controller lifecycle: setting up watches, reflectors,
/// and driving the reconciliation loop. Creates and owns the [`Reconciler`] internally,
/// delegating all reconciliation logic to it.
pub struct Controller {
    client: Client,
    reconciler: Arc<Reconciler>,
}

impl Controller {
    pub fn new(
        client: Client,
        scraper_tx: mpsc::Sender<EpaUpdate>,
        metrics_store: MetricsStore,
        epa_ownership: Arc<EpaOwnership>,
    ) -> Self {
        let reconciler = Arc::new(Reconciler::new(
            client.clone(),
            scraper_tx,
            metrics_store,
            epa_ownership,
        ));
        Self { client, reconciler }
    }

    /// Runs the controller loop.
    ///
    /// Starts the controller's watch loop and runs indefinitely until a shutdown signal is received.
    /// The controller watches all EPA resources cluster-wide and reconciles them on changes.
    ///
    /// Uses a reflector for efficient caching and only reconciles on generation changes to avoid
    /// unnecessary work.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on graceful shutdown, or an error if the controller fails to start.
    pub async fn run(self) -> anyhow::Result<()> {
        let epa_api = Api::<ExternalPodAutoscaler>::all(self.client);

        let (reader, writer) = reflector::store();

        let epa_stream = watcher(epa_api, watcher::Config::default())
            .default_backoff()
            .reflect(writer)
            .touched_objects()
            .predicate_filter(predicates::generation, Default::default());

        let reconcile_handler = self.reconciler.clone();
        let error_handler = self.reconciler.clone();

        KubeController::for_stream(epa_stream, reader)
            .shutdown_on_signal()
            .run(
                move |epa, ctx| {
                    let reconciler = reconcile_handler.clone();
                    async move { reconciler.reconcile(epa, ctx).await }
                },
                move |epa, err, ctx| error_handler.error_policy(epa, err, ctx),
                self.reconciler,
            )
            .for_each(|res| async move {
                match res {
                    Ok((_obj, _action)) => {
                        info!("Reconciled EPA successfully")
                    }
                    Err(err) => error!("Reconcile error: {}", err),
                }
            })
            .await;

        Ok(())
    }
}
