use std::{collections::HashMap, env};

use anyhow::Result;
use opentelemetry::{KeyValue, trace::TracerProvider};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{Resource, logs::SdkLoggerProvider, trace::SdkTracerProvider};
use tracing_subscriber::{
    EnvFilter, Layer as _,
    fmt::{Layer, time::LocalTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};
use uuid::Uuid;

use crate::config::Config;

// send traces to local elastic apm via OpenTelemetry
pub fn init(config: &Config) -> Result<()> {
    let filter = || EnvFilter::new("warn,tpn_bot=trace");

    let (otel_tracing, otel_logging) = match &config.otel {
        None => Default::default(),
        Some(otel) => {
            let resource = Resource::builder()
                .with_service_name("twitch-plays-noita")
                .with_attribute(KeyValue::new(
                    "deployment.environment",
                    config.env.to_string(),
                ))
                .with_attribute(KeyValue::new("session-id", Uuid::now_v7().to_string()))
                .build();
            let mut headers = HashMap::new();
            if let Some(auth) = &otel.auth_header {
                headers.insert("Authorization".to_owned(), auth.to_owned());
            }

            let logger_provider = SdkLoggerProvider::builder()
                .with_resource(resource.clone())
                .with_batch_exporter(
                    LogExporter::builder()
                        .with_http()
                        .with_endpoint(format!("{}/v1/logs", otel.url))
                        .with_headers(headers.clone())
                        .build()?,
                )
                .build();

            let trace_provider = SdkTracerProvider::builder()
                .with_resource(resource)
                .with_batch_exporter(
                    SpanExporter::builder()
                        .with_http()
                        .with_endpoint(format!("{}/v1/traces", otel.url))
                        .with_headers(headers)
                        .build()?,
                )
                .build();

            // see https://github.com/open-telemetry/opentelemetry-rust/blob/1d9bd25ec8974296b86770a016725ccce64a39b2/opentelemetry-appender-tracing/examples/basic.rs#L19-L37
            let filter_otel = filter()
                .add_directive("hyper=off".parse().unwrap())
                .add_directive("opentelemetry=off".parse().unwrap())
                .add_directive("tonic=off".parse().unwrap())
                .add_directive("h2=off".parse().unwrap())
                .add_directive("reqwest=off".parse().unwrap());

            (
                Some(
                    tracing_opentelemetry::layer()
                        .with_tracer(trace_provider.tracer(""))
                        .with_filter(filter()),
                ),
                Some(OpenTelemetryTracingBridge::new(&logger_provider).with_filter(filter_otel)),
            )
        }
    };

    // show our >=info in the terminal
    let fmt_layer = Layer::new()
        .with_timer(LocalTime::rfc_3339())
        .with_filter(EnvFilter::new(
            env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV)
                .as_deref()
                .unwrap_or("tpn_bot=info"),
        ));

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_tracing)
        .with(otel_logging)
        .try_init()?;
    Ok(())
}
