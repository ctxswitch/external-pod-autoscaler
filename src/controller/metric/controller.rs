use crate::apis::ctx_sh::v1beta1::Metric;
use crate::controller::metric::{Error};

use kube::{Api, Client, ResourceExt};
use kube_runtime::{
    controller::{Action, Controller as KubeController},
    predicates, reflector, watcher, WatchStreamExt,
};

use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, instrument};
use futures::StreamExt;

pub struct Controller {}

impl Controller {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn run(self: Arc<Self>, context: Arc<Context>) -> anyhow::Result<()> {
        let metrics_api = Api::<Metric>::all(context.client.clone());

        let (reader, writer) = reflector::store();

        let metrics_stream = watcher(metrics_api, watcher::Config::default())
            .default_backoff()
            .reflect(writer)
            .touched_objects()
            .predicate_filter(predicates::generation);

        let reconcile_handler = self.clone();
        let error_handler = self.clone();

        KubeController::for_stream(metrics_stream, reader)
            .shutdown_on_signal()
            .run(
                move |metric, ctx| {
                    let controller= reconcile_handler.clone();
                    async move { controller.reconcile(metric, ctx).await }
                },
                move |metric, err, ctx| {
                    error_handler.error_policy(metric, err, ctx)
                },
                context,
            )
            .for_each(|res| async move {
                match res {
                    Ok((obj, _action)) => info!("Reconciled: {:?}", obj),
                    Err(e) => error!("Reconcile error: {:?}", e),
                }
            })
            .await;

        Ok(())
    }

    #[instrument(skip(self, metric, ctx), fields(metric = %metric.name_any(), namespace = %metric.namespace().unwrap_or_default()))]
    pub async fn reconcile(
        &self,
        metric: Arc<Metric>,
        ctx: Arc<Context>,
    ) -> Result<Action, Error> {
        info!("Reconciling metric");
        // let _namespace = metric
        //     .metadata
        //     .namespace
        //     .as_deref()
        //     .unwrap_or_default()
        //     .to_string();

        // Do observer.

        let scrape_interval = metric.spec.target.interval_seconds as u64;
        Ok(Action::requeue(Duration::from_secs(scrape_interval)))
    }

    pub fn error_policy(
        &self,
        _metric: Arc<Metric>,
        error: &Error,
        _ctx: Arc<Context>,
    ) -> Action {
        error!("Metric reconciliation error: {:?}", error);
        // TODO: Make this an exponential backoff.
        Action::requeue(Duration::from_secs(5))
    }
}

pub struct Context {
    client: Client,
}

impl Context {
    pub fn new(client: Client) -> Self {
        Self {client}
    }
}
