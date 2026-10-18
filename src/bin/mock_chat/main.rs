//! The bot with the chat in this terminal instead of a Twitch channel.
//!
//! All the other services are the real ones (Valkey, Elastic, Jev, Noita,
//! sounds, TTS, the music player), except that Twitch writes (title,
//! shoutout, redemptions) only print what they would do. EventSub and Ko-fi
//! events come from terminal commands, see `/help`.

mod meta;
mod terminal;

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use clap::Parser;
use limatoukka::{
    bot::{self, BotServices, StateStore},
    commands::runner::Runner,
    config::Config,
    context::app::{AppContext, InterruptKind},
    events::{EventUser, handle_channel_event, handle_kofi_event, handle_noita_event},
    integration::twitch_api::TwitchApi,
    logging,
    services::{
        messaging::{Message, MessagingService, PermissionLevel, Sender},
        names::NamesServiceExt,
        storage::StorageServiceExt,
        twitch::{TwitchService, TwitchServiceExt, TwitchServiceImpl, UserIdResponse},
    },
};
use neca_cmd::Statement;
use tokio::{task::JoinSet, time::timeout};

use crate::{
    meta::{Action, HELP},
    terminal::{MessagingServiceTerminal, Output, TwitchServicePrinting},
};

/// Run the bot with the chat in this terminal instead of a Twitch channel.
#[derive(Parser)]
struct Args {
    /// The Valkey URL [default: `valkey` from the config]
    #[arg(long, conflicts_with = "in_memory")]
    valkey: Option<String>,

    /// Keep the state in memory instead of Valkey; there is no music player
    /// then
    #[arg(long)]
    in_memory: bool,

    /// The address of the browser sources (OBS pages)
    #[arg(long, default_value = "localhost:7457")]
    browser_source_bind: String,

    /// The address of the music player page
    #[arg(long, default_value = "localhost:35355")]
    music_player_bind: String,
}

fn prompt(sender: &Sender) -> String {
    let level: &str = sender.level.into();
    format!("{} [{}]", sender.name, level.to_lowercase())
}

struct Chat {
    ctx: AppContext,
    runner: Runner,
    output: Output,
    caster_login: String,
    sender: Sender,
    tasks: JoinSet<()>,
}

impl Chat {
    fn event_user(&self) -> EventUser {
        EventUser {
            id: self.sender.id.clone(),
            login: self.sender.login.clone(),
            name: self.sender.name.clone(),
        }
    }

    fn spawn(&mut self, task: impl Future<Output = Result<()>> + Send + 'static) {
        // forget the finished tasks
        while self.tasks.try_join_next().is_some() {}
        let output = self.output.clone();
        self.tasks.spawn(async move {
            if let Err(error) = task.await {
                output.error(&format!("error: {error:?}"));
            }
        });
    }

    /// Handle a line; returns `false` to quit.
    async fn handle(&mut self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return true;
        }
        let Some(command) = line.strip_prefix('/') else {
            self.chat(line);
            return true;
        };
        match meta::parse(command, &self.event_user()) {
            Ok(Action::Help) => self.output.note(HELP),
            Ok(Action::Quit) => return false,
            Ok(Action::As { login, level }) => {
                if let Err(error) = self.switch_user(&login, level).await {
                    self.output.error(&format!("{error:#}"));
                }
            }
            Ok(Action::Channel(event)) => {
                let ctx = self.ctx.clone();
                self.spawn(handle_channel_event(ctx, event));
            }
            Ok(Action::Noita(event)) => {
                let ctx = self.ctx.clone();
                self.spawn(handle_noita_event(ctx, event));
            }
            Ok(Action::Kofi(event)) => {
                let ctx = self.ctx.clone();
                self.spawn(handle_kofi_event(ctx, event));
            }
            Err(error) => self.output.error(&error),
        }
        true
    }

    async fn switch_user(&mut self, login: &str, level: Option<PermissionLevel>) -> Result<()> {
        let id = match self.ctx.twitch().get_user_id(login).await? {
            UserIdResponse::Found(id) => id.take(),
            UserIdResponse::NotFound => anyhow::bail!("user {login} not found"),
            UserIdResponse::Banned => anyhow::bail!("user {login} is banned"),
        };
        let name = self.ctx.names().lookup(&id).await?.into_owned();
        let level = level.unwrap_or(if login == self.caster_login {
            PermissionLevel::Caster
        } else {
            PermissionLevel::Viewer
        });
        if self.ctx.bot_id() == Some(&*id) {
            self.output
                .note("note: the bot ignores the messages of its own account");
        }
        self.sender = Sender {
            id,
            login: login.to_owned(),
            name,
            level,
        };
        Ok(())
    }

    fn chat(&mut self, text: &str) {
        let message = Message {
            id: uuid::Uuid::now_v7().to_string(),
            source_channel: self.caster_login.clone(),
            sender: self.sender.clone(),
            text: text.to_owned(),
            replyable: true,
        };
        let has_commands = !Statement::parse(text).is_noop();
        let error_key = format!("last-error:{}", message.sender.id);
        let ctx = self.ctx.clone();
        let runner = self.runner.clone();
        let output = self.output.clone();
        self.spawn(async move {
            runner.process_message(ctx.clone(), message).await?;
            // the chat sees no errors, but here they are useful
            if has_commands && let Some(error) = ctx.storage().get(&error_key).await? {
                output.error(&format!("last-error: {error}"));
            }
            Ok(())
        });
    }
}

async fn run(
    args: Args,
    output: Output,
    start_repl: impl FnOnce(String) -> Result<terminal::Repl>,
) -> Result<()> {
    let config = Config::load()?;

    let state = if args.in_memory {
        StateStore::InMemory
    } else {
        StateStore::connect(args.valkey.as_deref().unwrap_or(&config.valkey)).await?
    };

    let twitch_api = TwitchApi::new(&config).await?;

    let BotServices {
        injector,
        display_server,
        event_list,
        noita: noita_handle,
        music_player,
    } = bot::services(&config, &state, &twitch_api).await?;

    let bot_name = twitch_api.bot_login().to_owned();
    let services = injector
        .with::<dyn MessagingService>(Arc::new(MessagingServiceTerminal {
            output: output.clone(),
            bot_name,
        }))
        .with::<dyn TwitchService>(Arc::new(TwitchServicePrinting {
            inner: TwitchServiceImpl::new(twitch_api.clone()),
            output: output.clone(),
        }));

    let ctx = AppContext::new(services)
        .with_caster_id(twitch_api.caster_id().to_owned())
        .with_bot_id(twitch_api.bot_id().to_owned());

    let runner = bot::runner(&ctx);

    let server_output = output.clone();
    let display = display_server.start(&args.browser_source_bind, event_list.router());
    tokio::spawn(async move {
        if let Err(error) = display.await {
            server_output.error(&format!("browser sources are off: {error:#}"));
        }
    });
    if let Some(music_player) = &music_player {
        let server_output = output.clone();
        let player = music_player.start(&args.music_player_bind);
        tokio::spawn(async move {
            if let Err(error) = player.await {
                server_output.error(&format!("music player page is off: {error:#}"));
            }
        });
    }

    let mut noita_events = noita_handle.subscribe();
    tokio::spawn(noita_handle.poll_state_updates(ctx.clone()));

    let caster_login = twitch_api.caster_login().to_owned();
    let caster_name = ctx
        .names()
        .lookup(twitch_api.caster_id())
        .await?
        .into_owned();
    let mut chat = Chat {
        ctx: ctx.clone(),
        runner,
        output: output.clone(),
        sender: Sender {
            id: twitch_api.caster_id().to_owned(),
            login: caster_login.clone(),
            name: caster_name,
            level: PermissionLevel::Caster,
        },
        caster_login,
        tasks: JoinSet::new(),
    };

    let mut repl = start_repl(prompt(&chat.sender))?;
    output.note("Type chat messages, or /help for the terminal commands.");

    loop {
        tokio::select! {
            _ = ctx.wait_for_quit() => break,
            line = repl.lines.recv() => {
                let Some(line) = line else { break };
                let go_on = chat.handle(&line.text).await;
                _ = line.ack.send(prompt(&chat.sender));
                if !go_on {
                    break;
                }
            },
            Ok(event) = noita_events.recv() => {
                let ctx = ctx.clone();
                chat.spawn(handle_noita_event(ctx, event));
            },
        }
    }

    repl.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(repl.lines);
    _ = tokio::task::spawn_blocking(move || repl.thread.join()).await;

    while chat.tasks.try_join_next().is_some() {}
    if !chat.tasks.is_empty() {
        println!("interrupting the running commands...");
        ctx.interrupt(None, InterruptKind::Interrupt);
        let tasks = chat.tasks;
        if timeout(Duration::from_secs(3), tasks.join_all())
            .await
            .is_err()
        {
            println!("some commands did not stop in 3 s, quitting anyway");
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let history = PathBuf::from("target/mock-chat-history.txt");
    if let Some(dir) = history.parent() {
        std::fs::create_dir_all(dir).context("creating the history folder")?;
    }

    // the output waits for the line editor, which starts after the setup
    let (output, start_repl) = terminal::start(history);

    let _guard = logging::init_with_writer(output.clone(), "limatoukka=warn")?;

    run(args, output, start_repl).await
}
