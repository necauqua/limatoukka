//! The terminal side: the line editor, the output above it, and the
//! services that write to the terminal instead of Twitch.

use std::{
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self as std_mpsc, SyncSender},
    },
    thread::JoinHandle,
};

use anyhow::Result;
use async_trait::async_trait;
use limatoukka::services::{
    chat_box::{ChatBox, ChatBoxService, ChatMessage},
    messaging::{Message, MessagingService},
    twitch::{TwitchService, TwitchServiceImpl, UserIdResponse},
};
use reedline::{
    DefaultPrompt, DefaultPromptSegment, ExternalPrinter, FileBackedHistory, Reedline, Signal,
};
use tokio::sync::mpsc;
use tracing_subscriber::fmt::MakeWriter;

const BOLD_MAGENTA: &str = "\x1b[1;35m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// Prints lines above the line editor, or to stdout when the line editor
/// is closed.
#[derive(Clone)]
pub struct Output {
    printer: SyncSender<String>,
}

impl Output {
    pub fn print(&self, text: &str) {
        // the line editor prints each message as one line with \r\n
        for line in text.lines() {
            if let Err(unsent) = self.printer.send(line.to_owned()) {
                println!("{}", unsent.0);
            }
        }
    }

    /// A chat message of the bot.
    pub fn chat(&self, name: &str, text: &str) {
        self.print(&format!("{BOLD_MAGENTA}{name}{RESET}: {text}"));
    }

    /// A note from the terminal, not a chat message.
    pub fn note(&self, text: &str) {
        self.print(&format!("{DIM}{text}{RESET}"));
    }

    /// What the bot would do on Twitch.
    pub fn twitch(&self, text: &str) {
        self.print(&format!("{YELLOW}[twitch] {text}{RESET}"));
    }

    pub fn error(&self, text: &str) {
        self.print(&format!("{RED}{text}{RESET}"));
    }
}

/// Writes one log event, and sends it to the [`Output`] when dropped.
pub struct LogWriter {
    buffer: Vec<u8>,
    output: Output,
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.buffer);
        let text = text.trim_end();
        if !text.is_empty() {
            self.output.print(text);
        }
    }
}

impl<'w> MakeWriter<'w> for Output {
    type Writer = LogWriter;

    fn make_writer(&'w self) -> Self::Writer {
        LogWriter {
            buffer: Vec::new(),
            output: self.clone(),
        }
    }
}

/// A line from the line editor. Send the next prompt to `ack` when the line
/// is handled.
pub struct Line {
    pub text: String,
    pub ack: std_mpsc::Sender<String>,
}

pub struct Repl {
    pub lines: mpsc::UnboundedReceiver<Line>,
    /// Makes the line editor stop
    pub stop: Arc<AtomicBool>,
    pub thread: JoinHandle<()>,
}

/// Make the [`Output`], and the line editor thread that prints it. The
/// output is kept until the line editor starts.
pub fn start(history: PathBuf) -> (Output, impl FnOnce(String) -> Result<Repl>) {
    let printer = ExternalPrinter::<String>::new(1024);
    let output = Output {
        printer: printer.sender(),
    };

    let start = move |prompt: String| {
        let stop = Arc::new(AtomicBool::new(false));
        let mut editor = Reedline::create()
            .with_external_printer(printer)
            .with_break_signal(stop.clone())
            .with_history(Box::new(FileBackedHistory::with_file(1000, history)?));

        let (tx, lines) = mpsc::unbounded_channel();

        let thread_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut prompt = prompt;
            loop {
                let rendered = DefaultPrompt::new(
                    DefaultPromptSegment::Basic(prompt.clone()),
                    DefaultPromptSegment::Empty,
                );
                match editor.read_line(&rendered) {
                    Ok(Signal::Success(text)) => {
                        let (ack, next) = std_mpsc::channel();
                        if tx.send(Line { text, ack }).is_err() {
                            break;
                        }
                        match next.recv() {
                            Ok(next) => prompt = next,
                            Err(_) => break,
                        }
                    }
                    Ok(Signal::CtrlC | Signal::CtrlD | Signal::ExternalBreak(_)) => break,
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!("line editor error: {error}");
                        break;
                    }
                }
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        });

        Ok(Repl {
            lines,
            stop,
            thread,
        })
    };

    (output, start)
}

/// Clears the chat box, but does not store the time of the clear: the
/// backlog of the bot (which can use the same storage) stays as it is.
pub struct ChatBoxTerminal(pub Arc<ChatBox>);

#[async_trait]
impl ChatBoxService for ChatBoxTerminal {
    async fn clear(&self) -> Result<()> {
        self.0.clear_shown();
        Ok(())
    }
}

/// Writes the chat messages of the bot to the terminal and to the chat box.
pub struct MessagingServiceTerminal {
    pub output: Output,
    pub bot_id: String,
    pub bot_name: String,
    pub chat_box: Arc<ChatBox>,
}

impl MessagingServiceTerminal {
    fn to_chat_box(&self, text: &str) {
        let message = ChatMessage::new(&self.bot_id, &self.bot_name, &self.bot_name, text);
        self.chat_box.push(message);
    }
}

#[async_trait]
impl MessagingService for MessagingServiceTerminal {
    async fn send(&self, text: String) -> Result<()> {
        self.output.chat(&self.bot_name, &text);
        self.to_chat_box(&text);
        Ok(())
    }

    async fn reply(&self, message: &Message, text: String) -> Result<()> {
        self.output.chat(
            &self.bot_name,
            &format!("{DIM}(@{}){RESET} {text}", message.sender.name),
        );
        self.to_chat_box(&text);
        Ok(())
    }
}

/// The real Twitch API for reads; the writes only print what they would do.
pub struct TwitchServicePrinting {
    pub inner: TwitchServiceImpl,
    pub output: Output,
}

#[async_trait]
impl TwitchService for TwitchServicePrinting {
    async fn is_live(&self) -> Result<bool> {
        self.inner.is_live().await
    }

    async fn get_user_id(&self, login: &str) -> Result<UserIdResponse> {
        self.inner.get_user_id(login).await
    }

    async fn set_stream_title(&self, title: &str) -> Result<()> {
        self.output
            .twitch(&format!("would set the stream title to {title:?}"));
        Ok(())
    }

    async fn shout_out(&self, user_id: &str) -> Result<()> {
        self.output
            .twitch(&format!("would shout out the user with id {user_id}"));
        Ok(())
    }

    async fn fulfill_redemption(&self, reward_id: &str, id: &str) -> Result<()> {
        self.output.twitch(&format!(
            "would fulfill the redemption {id} of the reward {reward_id}"
        ));
        Ok(())
    }

    async fn followed_at(&self, user_id: &str) -> Result<Option<jiff::Timestamp>> {
        self.inner.followed_at(user_id).await
    }
}
