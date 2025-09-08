use std::{future::pending, process::Stdio};

use anyhow::{Result, bail};
use tokio::{
    process::{Child, Command},
    sync::oneshot::Receiver,
};

// eh I couldnt be bothered lol
pub async fn just_detached(script: &str, extra_args: &[&str]) -> Result<()> {
    // we just send it and *dont* wait for like noita.exe to finish
    let _res = Command::new("setsid")
        .args(["just", script])
        .args(extra_args)
        .env_remove("RUST_LOG")
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    //     .wait_with_output()
    //     .await?;
    // if !res.status.success() {
    //     bail!(
    //         "just command failed: {}",
    //         String::from_utf8_lossy(&res.stderr)
    //     )
    // }
    Ok(())
}

pub fn just(script: &str, extra_args: &[&str]) -> Result<Process> {
    Ok(Process {
        child: Command::new("setsid")
            .args(["just", script])
            .args(extra_args)
            .env_remove("RUST_LOG")
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?,
    })
}

pub struct Process {
    child: Child,
}

impl Process {
    pub async fn get(self) -> Result<Result<String, String>> {
        let output = self.child.wait_with_output().await?;
        if output.status.success() {
            Ok(Ok(String::from_utf8(output.stdout)?))
        } else {
            Ok(Err(String::from_utf8(output.stderr)?))
        }
    }

    pub async fn check(self) -> Result<String> {
        match self.get().await? {
            Ok(success) => Ok(success),
            Err(err) => bail!("just command failed: {err}"),
        }
    }

    async fn do_wait(&mut self) {
        if let Err(e) = self.child.wait().await {
            tracing::error!(?e, "child process errored");
        }
    }

    pub async fn wait(&mut self, stop: Option<Receiver<()>>) -> bool {
        let stop = async {
            if let Some(stop) = stop {
                _ = stop.await;
            } else {
                pending::<()>().await;
            }
        };
        tokio::select! {
            _ = self.do_wait() => true,
            _ = stop => {
                // ugh meh
                let pid = self.child.id().unwrap();
                unsafe { libc::killpg(pid as _, libc::SIGTERM) };
                self.do_wait().await;
                false
            },
        }
    }
}
