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
    let storage = Storage::new(&config).await?;
    let xdo = XDoClient::new(config.display.clone());

    let (twitch, eventsub) = Twitch::new(&config.twitch).await?;
    let (mut incoming, messaging) = messaging::connect_to_twitch(twitch.clone());

    let mut eventsub_rx = eventsub.subscribe();

    tokio::spawn(async {
        if let Err(e) = eventsub.run().await {
            tracing::error!(error=?e, "twitch eventsub fail");
        }
    });

    let ctx = AppContext::new(
        messaging,
        config,
        storage,
        xdo,
        NoitaHandle::default(),
        StatusWall::default(),
        twitch,
    );

    ctx.init();

    let mut found_items = ctx.noita().subscribe_to_found_items();

    loop {
        let res = tokio::select! {
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
                Ok(())
            }
            _ = ctx.noita().wait_for_player_death() => ctx.next_run().await,
            Ok(found) = found_items.recv() => {
                ctx.send(match found {
                    ItemFound::TreeTablet => "The best TABLET in the game acquired!",
                    ItemFound::OtherTablet => "TABLET acquired",
                    ItemFound::EvilEye => "Got the EVILEYE",
                    ItemFound::EarthStone => {
                        "The final frontier before all the wacky shit, EARTHSTONE acquired! POGGIES"
                    }
                    ItemFound::TouchOfGold => {
                        "TOUCHOFGOLD - Infinite money glitch? Midas at home? A boss-killer even ( Clueless )?"
                    }
                    ItemFound::Taikasauva => {
                        "Got the SUMMONTAIKASAUVA , the whole world is in your hands now"
                    }
                }.into()).await
            }
            Ok(event) = eventsub_rx.recv() => ctx.handle_event(event).await,
            else => return Ok(()),
        };
        if let Err(e) = res {
            tracing::error!(error = ?e, "main loop error");
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
