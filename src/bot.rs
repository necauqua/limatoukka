//! The service setup that the Twitch bot and the terminal bot share.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use rustis::client::Client as ValkeyClient;

use crate::{
    commands::{CommandTag, discover_declared_commands, runner::Runner},
    config::Config,
    context::app::AppContext,
    integration::{twitch_api::TwitchApi, yt_music_api::YouTubeMusic},
    services::{
        Injector,
        banishes::{BanishService, BanishServiceInMemory, BanishServiceValkey},
        caches::{CacheService, CacheServiceInMemory, CacheServiceValkey},
        charges::{ChargesService, ChargesServiceInMemory, ChargesServiceValkey},
        chat_log::{ChatLogService, ChatLogServiceElastic},
        display::{DisplayServer, DisplayService, DisplayServiceAux},
        gates::{GateService, GateServiceInMemory, GateServiceValkey},
        ipc::{IpcService, IpcServiceInMemory, IpcServiceValkey},
        music::{MusicService, MusicServiceUnavailable, YouTubeMusicPlayer},
        names::{NamesService, NamesServiceImpl},
        noita::{NoitaHandle, NoitaService},
        sounds::{SoundService, SoundServiceImpl},
        stats::{StatsService, StatsServiceElastic},
        status_wall::{StatusService, StatusWall},
        storage::{InMemoryStorageService, StorageService, StorageServiceValkey},
        tts::{TtsService, TtsServiceImpl},
        variables::{VariableStorage, VariableStorageInMemory, VariableStorageValkey},
    },
};

/// Where the bot keeps its state.
pub enum StateStore {
    Valkey(ValkeyClient),
    /// Nothing persists, and there is no music player.
    InMemory,
}

impl StateStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = ValkeyClient::connect(url)
            .await
            .context("connecting to db")?;
        Ok(Self::Valkey(client))
    }
}

pub struct BotServices {
    /// Has all the services, except [`MessagingService`] and
    /// [`TwitchService`].
    ///
    /// [`MessagingService`]: crate::services::messaging::MessagingService
    /// [`TwitchService`]: crate::services::twitch::TwitchService
    pub injector: Injector,
    pub display_server: Arc<DisplayServer>,
    pub noita: Arc<NoitaHandle>,
    /// `None` with [`StateStore::InMemory`]
    pub music_player: Option<Arc<YouTubeMusicPlayer>>,
}

pub async fn services(
    config: &Config,
    state: &StateStore,
    twitch_api: &TwitchApi,
) -> Result<BotServices> {
    let display_server = Arc::new(DisplayServer::default());
    let noita = Arc::new(NoitaHandle::default());

    let (injector, caches, music_player) = match state {
        StateStore::Valkey(valkey) => {
            let caches: Arc<dyn CacheService> = Arc::new(CacheServiceValkey::new(valkey.clone()));
            let music_player = Arc::new(
                YouTubeMusicPlayer::new(
                    YouTubeMusic::new(
                        config.youtube.api_key.clone(),
                        config.youtube.country_code.clone(),
                    ),
                    &config.youtube.playlist,
                    valkey.clone(),
                    display_server.clone(),
                )
                .await?,
            );
            let injector = Injector::new()
                .with::<dyn StorageService>(Arc::new(StorageServiceValkey::new(valkey.clone())))
                .with::<dyn VariableStorage>(Arc::new(VariableStorageValkey::new(valkey.clone())))
                .with::<dyn IpcService>(Arc::new(IpcServiceValkey::new(valkey.clone())))
                .with::<dyn BanishService>(Arc::new(BanishServiceValkey::new(valkey.clone())))
                .with::<dyn ChargesService>(Arc::new(ChargesServiceValkey::new(valkey.clone())))
                .with::<dyn GateService>(Arc::new(GateServiceValkey::new(valkey.clone())))
                .with::<dyn MusicService>(music_player.clone());
            (injector, caches, Some(music_player))
        }
        StateStore::InMemory => {
            let caches: Arc<dyn CacheService> = Arc::new(CacheServiceInMemory::default());
            let injector = Injector::new()
                .with::<dyn StorageService>(Arc::new(InMemoryStorageService::default()))
                .with::<dyn VariableStorage>(Arc::new(VariableStorageInMemory::default()))
                .with::<dyn IpcService>(Arc::new(IpcServiceInMemory::default()))
                .with::<dyn BanishService>(Arc::new(BanishServiceInMemory::default()))
                .with::<dyn ChargesService>(Arc::new(ChargesServiceInMemory::default()))
                .with::<dyn GateService>(Arc::new(GateServiceInMemory::default()))
                .with::<dyn MusicService>(Arc::new(MusicServiceUnavailable(
                    "the state is in memory",
                )));
            (injector, caches, None)
        }
    };

    let injector = injector
        .with::<dyn CacheService>(caches.clone())
        .with::<dyn StatsService>(Arc::new(StatsServiceElastic::new(
            &config.stats.url,
            &config.stats.api_key,
            &config.stats.index,
        )?))
        .with::<dyn ChatLogService>(Arc::new(ChatLogServiceElastic::new(
            &config.elastic.url,
            &config.elastic.api_key,
            &config.elastic.index,
        )?))
        .with::<dyn SoundService>(Arc::new(SoundServiceImpl::default()))
        .with::<dyn TtsService>(Arc::new(TtsServiceImpl::default()))
        .with::<dyn StatusService>(Arc::new(StatusWall::new(display_server.wrap("status"))))
        .with::<dyn DisplayService>(display_server.clone())
        .with::<dyn NoitaService>(noita.clone())
        .with::<dyn NamesService>(Arc::new(NamesServiceImpl::new(caches, twitch_api.clone())));

    Ok(BotServices {
        injector,
        display_server,
        noita,
        music_player,
    })
}

/// The command runner with all the enabled commands.
pub fn runner(ctx: &AppContext) -> Runner {
    let mut commands = discover_declared_commands();

    // todo make this less cringe
    commands.retain(|_, v| !v.is(CommandTag::NoitaControl));

    Runner::new(commands, ctx)
}
