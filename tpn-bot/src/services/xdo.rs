use std::io::BufRead;

use anyhow::{Context, Result, bail};
use futures::TryFutureExt;
use tokio::process::Command;

pub struct XDoClient {
    display: Option<String>,
}

macro_rules! xdotool_calls {
    (__tostring $arg:ident &str) => { $arg };
    (__tostring $arg:ident $tpe:ty) => { &$arg.to_string() };
    ($($name:ident($($arg:ident: $tpe:ty),*);)*) => {
        $(
            pub fn $name(&self, $($arg: $tpe),*) -> impl Future<Output = Result<()>> + use<> {
                let f = self.cmd(&[stringify!($name), "--", $( xdotool_calls!(__tostring $arg $tpe) ),* ]);
                async {
                    f.await?;
                    Ok(())
                }
            }
        )*
    }
}

impl XDoClient {
    pub fn new(display: Option<String>) -> Self {
        Self { display }
    }

    // xdotool uses xlib, so if you use libxdo and the X connection drops
    // (because, say, you're spawning secondary ones to run the game offscreen)
    // xlib ABORTS THE FUCKING PROCESS LMAO
    // so for now we just shut up and shell out to xdotool, meh
    //
    // at least it's way simpler than managing mpsc channels feeding a single
    // xdo instance or whatever (I totally did not have all that implemented)
    fn cmd(&self, args: &[&str]) -> impl Future<Output = Result<Vec<u8>>> + use<> {
        tracing::debug!("xdotool: {args:?}");
        Command::new("xdotool")
            .env("DISPLAY", self.display.as_deref().unwrap_or(":0"))
            .args(args)
            .output()
            .map_err(|e| e.into())
            .and_then(|result| async move {
                if !result.status.success() {
                    bail!("{}", String::from_utf8_lossy(&result.stderr))
                }
                Ok(result.stdout)
            })
    }

    pub fn getmouselocation(&self) -> impl Future<Output = Result<(u32, u32)>> + use<> {
        let f = self.cmd(&["getmouselocation", "--shell"]);
        async {
            let output = f.await?;
            let mut lines = output.lines();
            let x = lines
                .next()
                .and_then(|l| l.ok())
                .and_then(|l| l.strip_prefix("X=").and_then(|l| l.parse().ok()))
                .context("bad mouse location output")?;
            let y = lines
                .next()
                .and_then(|l| l.ok())
                .and_then(|l| l.strip_prefix("Y=").and_then(|l| l.parse().ok()))
                .context("bad mouse location output")?;
            Ok((x, y))
        }
    }

    xdotool_calls! {
        click(button: i32);
        mousedown(button: i32);
        mouseup(button: i32);
        mousemove(x: i32, y: i32);
        mousemove_relative(x: i32, y: i32);
        key(key: &str);
        keydown(key: &str);
        keyup(key: &str);
    }
}
