use anyhow::Result;
use opentelemetry::trace::Status;
use tpn_bot::{
    commands::{
        context::{AppContext, Valkey},
        runner,
    },
    config::Config,
    logging,
    services::{
        messaging,
        noita::{ItemFound, NoitaHandle},
        status_wall::StatusWall,
        twitch::Twitch,
        xdo::XDoClient,
    },
};

use tracing::{Instrument, Span, field::Empty};
use tracing_opentelemetry::OpenTelemetrySpanExt;

async fn run(config: Config) -> Result<()> {
    // let it fail before we connect to twitch
    let valkey = Valkey::connect(&*config.valkey).await?;

    let twitch = Twitch::new(&config.twitch).await?;
    let (mut incoming, messaging) = {
        // Some(twitch) => {
        messaging::connect_to_twitch(twitch.clone())
        // }
        // _ => messaging::connect_to_mock().await?,
    };

    let status_wall = StatusWall::default();
    tokio::spawn(status_wall.start(&config.browser_source_bind));

    let xdo = XDoClient::new(config.display.clone());
    // fix any stuck holds
    tokio::spawn({
        let xdo = xdo.clone();
        async move {
            tokio::join!(
                xdo.keyup("w"),
                xdo.keyup("a"),
                xdo.keyup("s"),
                xdo.keyup("d"),
                xdo.mouseup(1),
            )
        }
    });

    let noita = NoitaHandle::default();
    tokio::spawn(noita.clone().poll_state_updates());

    let ctx = AppContext::new(
        messaging,
        config,
        valkey,
        xdo,
        noita,
        status_wall,
        Default::default(),
        twitch,
    );

    let mut rx = ctx.noita().subscribe_to_found_items();

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
            Ok(found) = rx.recv() => {
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
