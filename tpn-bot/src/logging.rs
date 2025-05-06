use anyhow::Result;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use tracing_loki::url::Url;
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::config::Config;

// we persist logs in loki, but also send them to an opentelemetry collector for nice trace visualization
pub fn init(config: &Config) -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::Layer::new().with_filter(
                tracing_subscriber::EnvFilter::builder().parse(
                    std::env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV)
                        .as_deref()
                        .unwrap_or("tpn_bot=info"),
                )?,
            ),
        )
        .with(match config.loki.as_deref() {
            Some(loki) => {
                let session_id = Uuid::now_v7();
                let (layer, task) = tracing_loki::builder()
                    .label("job", "twitch-plays-noita")?
                    .label("env", config.env.to_string())?
                    .extra_field("session_id", session_id.to_string())?
                    .build_url(Url::parse(loki).unwrap())?;

                tokio::spawn(task);
                Some(layer)
            }
            None => None,
        })
        .with(match config.otel.as_deref() {
            Some(otel) => {
                let provider = SdkTracerProvider::builder()
                    .with_batch_exporter(
                        opentelemetry_otlp::HttpExporterBuilder::default()
                            .with_endpoint(otel)
                            .build_span_exporter()?,
                    )
                    .with_resource(
                        Resource::builder()
                            .with_service_name("twitch-plays-noita")
                            .build(),
                    )
                    .build();
                let tracer = provider.tracer("twitch-plays-noita");
                Some(tracing_opentelemetry::layer().with_tracer(tracer))
            }
            None => None,
        })
        .try_init()?;
    Ok(())
}
