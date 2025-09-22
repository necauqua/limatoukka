use std::env;

use anyhow::Result;
use tracing_chrome::{ChromeLayerBuilder, FlushGuard, TraceStyle};
use tracing_subscriber::{
    EnvFilter, Layer as _,
    fmt::{Layer, time::LocalTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

pub struct LoggingGuard {
    #[allow(unused)] // this is a drop guard
    chrome_guard: FlushGuard,
}

pub fn init() -> Result<LoggingGuard> {
    let (chrome_layer, chrome_guard) = ChromeLayerBuilder::new()
        .include_args(true)
        .trace_style(TraceStyle::Async)
        // todo somehow extract subname from spans:
        // .name_fn(Box::new(|event_or_span| match event_or_span {
        //     EventOrSpan::Event(ev) => ev.metadata().name().into(),
        //     EventOrSpan::Span(s) => match s.fields().field("subname") {
        //         Some(f) => {
        //             format!("{}: {}", s.name(), f.name())
        //         }
        //         None => s.name().into(),
        //     },
        // }))
        .build();

    // send our >=info to the stdout
    let fmt_layer = Layer::new()
        .with_timer(LocalTime::rfc_3339())
        .with_filter(EnvFilter::new(
            env::var(tracing_subscriber::EnvFilter::DEFAULT_ENV)
                .as_deref()
                .unwrap_or("limatoukka=info"),
        ));

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(chrome_layer)
        .try_init()?;

    Ok(LoggingGuard { chrome_guard })
}
