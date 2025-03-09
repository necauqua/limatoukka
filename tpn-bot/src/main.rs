use std::sync::Arc;

use anyhow::Result;
use tpn_bot::{
    commands::{
        context::{AppContext, Valkey},
        runner,
    },
    config::Config,
    logging,
    services::{messaging, noita::NoitaHandle, status_wall::StatusWall, xdo::XDoClient},
};

use tracing::{Instrument, field::Empty};
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
        _ => {
            messaging::connect_to_mock(messaging::Sender {
                id: "mock".into(),
                login: "mock".into(),
                name: "mock".into(),
            })
            .await?
        }
    };

    let status_wall = StatusWall::new();
    tokio::spawn(status_wall.start_server(&config.browser_source_bind));

    _ = status_wall.push("hello".to_owned());

    let xdo = XDoClient::new(config.display.clone());
    let noita = NoitaHandle::new();

    let state = AppContext {
        config: config.into(),
        messaging,
        storage: Arc::new(valkey),
        xdo,
        noita,
        status_wall,
    };

    // the main loop lol
    tokio::spawn({
        let state = state.clone();
        async move {
            loop {
                state.noita.wait_for_player_death().await;
                if let Err(error) = state.next_run().await {
                    tracing::error!(?error, "failed to start next run");
                }
            }
        }
    });

    // the _other_ main loop lol²
    while let Some(message) = incoming.recv().await {
        tokio::spawn(runner::receive_message(state.clone(), message));
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
