use anyhow::Result;
use opentelemetry::trace::Status;
use tpn_bot::{
    commands::runner,
    config::Config,
    context::app::AppContext,
    logging,
    services::{
        messaging,
        noita::{ItemFound, NoitaHandle},
        status_wall::StatusWall,
        storage::Storage,
        twitch::Twitch,
        xdo::XDoClient,
    },
};

use tracing::{Instrument, Span, field::Empty};
use tracing_opentelemetry::OpenTelemetrySpanExt;

async fn run(config: Config) -> Result<()> {
    let storage = Storage::new(&*config.valkey).await?;
    let twitch = Twitch::new(&config.twitch).await?;
    let xdo = XDoClient::new(config.display.clone());

    let (mut incoming, messaging) = messaging::connect_to_twitch(twitch.clone());

    let ctx = AppContext::new(
        messaging,
        config,
        storage,
        xdo,
        NoitaHandle::default(),
        StatusWall::default(),
        Default::default(),
        twitch,
    );

    ctx.init();

    let mut found_items = ctx.noita().subscribe_to_found_items();

    loop {
        tokio::select! {
            Some(msg) = incoming.recv() => {
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
            _ = ctx.noita().wait_for_player_death() => {
                if let Err(error) = ctx.next_run().await {
                    tracing::error!(?error, "failed to start next run");
                }
            }
            Ok(found) = found_items.recv() => {
                let message = match found {
                    ItemFound::TreeTablet => "The best TABLET in the game acquired!",
                    ItemFound::OtherTablet => "TABLET acquired",
                    ItemFound::EvilEye => "Got the EVILEYE",
                    ItemFound::EarthStone => {
                        "The final frontier before all the wacky shit, EARTHSTONE acquired! POGGIES"
                    }
                    ItemFound::Taikasauva => {
                        "Got the SUMMONTAIKASAUVA , the whole world is in your hands now"
                    }
                };
                if let Err(error) = ctx.send(message.into()).await {
                    tracing::error!(?error, "failed send item found message");
                }
            }
            else => return Ok(()),
        }
    }
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
