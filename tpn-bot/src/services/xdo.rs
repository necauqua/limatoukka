use std::sync::Arc;

use anyhow::{Result, bail};
use tokio::process::Command;

#[derive(Clone)]
pub struct XDoClient {
    display: Option<Arc<str>>,
}

macro_rules! xdotool_calls {
    (__tostring $arg:ident &str) => { $arg };
    (__tostring $arg:ident $tpe:ty) => { &$arg.to_string() };
    ($($name:ident($($arg:ident: $tpe:ty),*);)*) => {
        $(
            // #[must_use]
            pub async fn $name(&self, $($arg: $tpe),*) -> Result<()> {
                self.cmd(&[stringify!($name), $( xdotool_calls!(__tostring $arg $tpe) ),* ]).await
            }
        )*
    }
}

impl XDoClient {
    pub fn new(display: Option<String>) -> Self {
        Self {
            display: display.map(Arc::from),
        }
    }

    // xdotool uses xlib, so if you use libxdo and the X connection drops
    // (because, say, you're spawning secondary ones to run the game offscreen)
    // xlib ABORTS THE FUCKING PROCESS LMAO
    // so for now we just shut up and shell out to xdotool, meh
    //
    // at least it's way simpler than managing mpsc channels feeding a single
    // xdo instance or whatever (I totally did not have all that implemented)
    async fn cmd(&self, args: &[&str]) -> Result<()> {
        tracing::debug!(?args, "running xdotool");
        let result = Command::new("xdotool")
            .env("DISPLAY", self.display.as_deref().unwrap_or(":0"))
            .args(args)
            .output()
            .await?;
        if !result.status.success() {
            bail!("{}", String::from_utf8_lossy(&result.stderr))
        }
        Ok(())
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
