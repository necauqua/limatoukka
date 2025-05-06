use std::{collections::HashMap, env};

use anyhow::Result;
use opentelemetry::{KeyValue, trace::TracerProvider};
use opentelemetry_otlp::{SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use tracing_subscriber::{
    EnvFilter, Layer as _, fmt::Layer, layer::SubscriberExt, util::SubscriberInitExt,
};
use uuid::Uuid;

use crate::config::Config;

// send traces to local elastic apm via OpenTelemetry
pub fn init(config: &Config) -> Result<()> {
    let otel_tracing = match &config.otel {
        None => None,
        Some(otel) => {
            let resource = Resource::builder()
                .with_service_name("twitch-plays-noita")
                .with_attribute(KeyValue::new(
                    "deployment.environment",
                    config.env.to_string(),
                ))
                .with_attribute(KeyValue::new(
                    "agent.ephemeral_id",
                    Uuid::now_v7().to_string(),
                ))
                .build();

            let trace_provider = SdkTracerProvider::builder()
                .with_resource(resource)
                .with_batch_exporter(
                    SpanExporter::builder()
                        .with_http()
                        .with_endpoint(format!("{}/v1/traces", otel.url))
                        .with_headers({
                            let mut headers = HashMap::new();
                            if let Some(auth) = &otel.auth_header {
                                println!("Auth header: {auth}");
                                headers.insert("Authorization".to_owned(), auth.to_owned());
                            } else {
                                println!("No auth header provided");
                            }
                            headers
                        })
                        .build()?,
                )
                .build();

            Some(tracing_opentelemetry::layer().with_tracer(trace_provider.tracer("")))
        }
    };

    // show our >=info in the terminal
    let fmt_layer = Layer::new().with_filter(EnvFilter::new(
        env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV)
            .as_deref()
            .unwrap_or("tpn_bot=info"),
    ));

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_tracing)
        .try_init()?;
    Ok(())
}
