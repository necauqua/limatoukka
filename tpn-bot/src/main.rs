use std::sync::Arc;

use anyhow::Result;
use opentelemetry::trace::Status;
use tpn_bot::{
    commands::{
        context::{AppContext, Valkey},
        runner,
    },
    config::Config,
    logging,
    services::{messaging, noita::NoitaHandle, status_wall::StatusWall, xdo::XDoClient},
};

use tracing::{Instrument, Span, field::Empty};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use twitch_irc::login::StaticLoginCredentials;

async fn run(config: Config) -> Result<()> {
    // let it fail before we connect to twitch
    let valkey = Valkey::connect(&*config.valkey).await?;

    let (mut incoming, messaging) = match &config.bot {
        Some(bot) => {
            let creds =
                StaticLoginCredentials::new(bot.login.to_owned(), Some(bot.token.to_owned()));
            messaging::connect_to_twitch(creds, bot.target.to_owned())
        }
        _ => messaging::connect_to_mock().await?,
    };

    let status_wall = StatusWall::new();
    tokio::spawn(status_wall.start(&config.browser_source_bind));

    let xdo = XDoClient::new(config.display.clone());
    let noita = NoitaHandle::new();

    let ctx = AppContext {
        config: config.into(),
        messaging,
        storage: Arc::new(valkey),
        xdo,
        noita,
        status_wall,
        holds: Default::default(),
    };

    // the main loop, lol
    tokio::spawn({
        let state = ctx.clone();
        async move {
            loop {
                state.noita.wait_for_player_death().await;
                if let Err(error) = state.next_run().await {
                    tracing::error!(?error, "failed to start next run");
                }
            }
        }
    });

    // the _other_ main loop, lol²
    while let Some(msg) = incoming.recv().await {
        let span = tracing::info_span!(
            "message",
            msg.id,
            msg.text,
            msg.sender = msg.sender.login,
            msg.sender.id = msg.sender.id,
        );
        let ctx = ctx.clone();
        tokio::spawn(
            async move {
                if let Err(error) = runner::receive_message(ctx, msg).await {
                    tracing::error!(?error, "failed to handle message");
                    Span::current().set_status(Status::error("error"));
                }
            }
            .instrument(span),
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load()?;
    logging::init(&config)?;

    tracing::info!("started");

    run(config)
        .instrument(tracing::info_span!("run", run.seed = Empty))
        .await
}
