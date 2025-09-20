use std::env;

use anyhow::Result;
use tracing_subscriber::{
    EnvFilter, Layer as _,
    fmt::{Layer, time::LocalTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

pub fn init() -> Result<()> {
    // send our >=info to the stdout
    let fmt_layer = Layer::new()
        .with_timer(LocalTime::rfc_3339())
        .with_filter(EnvFilter::new(
            env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV)
                .as_deref()
                .unwrap_or("limatoukka=debug"),
        ));

    tracing_subscriber::registry().with(fmt_layer).try_init()?;

    Ok(())
}
