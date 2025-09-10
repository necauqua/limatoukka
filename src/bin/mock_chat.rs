use std::fs::OpenOptions;
use std::io::Write;

use anyhow::{Result, bail};
use reedline::{DefaultPrompt, Reedline, Signal};

fn main() -> Result<()> {
    let mut line_editor = Reedline::create();

    let prompt = DefaultPrompt {
        left_prompt: reedline::DefaultPromptSegment::Basic("noita".into()),
        ..Default::default()
    };

    let mut tx = OpenOptions::new().write(true).open("/tmp/tpn-bot.fifo")?;

    loop {
        let sig = line_editor.read_line(&prompt);
        match sig {
            Ok(Signal::Success(buffer)) => writeln!(tx, "{buffer}")?,
            Ok(Signal::CtrlD) | Ok(Signal::CtrlC) => break Ok(()),
            Err(e) => bail!(e),
        }
    }
}
