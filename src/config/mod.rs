mod def;

use std::fmt::Display;

use anyhow::Result;
use config::File;

pub use def::*;

impl Display for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Env::Dev => write!(f, "dev"),
            Env::Prod => write!(f, "prod"),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let cfg = config::Config::builder()
            .add_source(File::with_name("conf/config"))
            .add_source(File::with_name("conf/config.private").required(false))
            .build()?
            .try_deserialize()?;
        Ok(cfg)
    }
}
