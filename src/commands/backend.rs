use std::{collections::HashMap, sync::Arc};

use anyhow::{Result, bail};
use async_trait::async_trait;
use dashmap::DashMap;
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::{GenericCommands, HashCommands},
};
use thiserror::Error;

use crate::commands::NativeCommand;

#[derive(Debug, Clone)]
pub enum MacroResolution {
    None,
    Personal(String),
    Global(String),
}

#[derive(Error, Debug)]
pub enum CreateMacroError {
    #[error("Macro limit reached")]
    TooManyMacros,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

impl From<rustis::Error> for CreateMacroError {
    fn from(e: rustis::Error) -> Self {
        Self::Internal(anyhow::anyhow!(e))
    }
}

#[derive(Error, Debug)]
pub enum SetVarError {
    #[error("Variable limit reached")]
    TooManyVars,
    #[error("internal")]
    Internal(#[from] anyhow::Error),
}

impl From<rustis::Error> for SetVarError {
    fn from(e: rustis::Error) -> Self {
        Self::Internal(anyhow::anyhow!(e))
    }
}

#[async_trait]
pub trait RunnerBackend: Send + Sync {
    async fn get_native_command(&self, command: &str) -> Result<Option<Arc<NativeCommand>>>;

    async fn create_macro(
        &self,
        owner: &str,
        name: &str,
        script: &str,
    ) -> Result<(), CreateMacroError>;

    async fn get_macro(&self, owner: &str, name: &str) -> Result<Option<String>>;

    async fn list_macros(&self, owner: &str) -> Result<Vec<(String, String)>>;

    async fn delete_macro(&self, owner: &str, name: &str) -> Result<bool>;

    async fn create_global_macro(&self, name: &str, script: &str) -> Result<()>;

    async fn get_global_macro(&self, name: &str) -> Result<Option<String>>;

    async fn list_global_macros(&self) -> Result<Vec<(String, String)>>;

    async fn delete_global_macro(&self, name: &str) -> Result<bool>;

    async fn resolve_macro(&self, owner: &str, command: &str) -> Result<MacroResolution>;

    async fn set_var(&self, owner: &str, name: &str, value: &str) -> Result<(), SetVarError>;

    async fn del_vars(&self, owner: &str, keys: &[&str]) -> Result<usize>;

    async fn list_vars(&self, owner: &str) -> Result<Vec<String>>;

    async fn clear_vars(&self, owner: &str) -> Result<()>;

    async fn set_global_var(&self, name: &str, value: &str) -> Result<()>;

    async fn load_vars(&self, owner: &str) -> Result<DashMap<String, String>>;
}

pub struct RunnerBackendRedis {
    client: ValkeyClient,
    commands: HashMap<String, Arc<NativeCommand>>,
}

impl RunnerBackendRedis {
    pub fn new(client: ValkeyClient, commands: HashMap<String, Arc<NativeCommand>>) -> Self {
        Self { client, commands }
    }
}

pub struct RunnerBackendStatic {
    commands: HashMap<String, Arc<NativeCommand>>,
}

impl RunnerBackendStatic {
    pub fn new(commands: HashMap<String, Arc<NativeCommand>>) -> Self {
        Self { commands }
    }
}

#[async_trait]
impl RunnerBackend for RunnerBackendStatic {
    async fn get_native_command(&self, command: &str) -> Result<Option<Arc<NativeCommand>>> {
        Ok(self.commands.get(command).cloned())
    }

    async fn create_macro(
        &self,
        _owner: &str,
        _name: &str,
        _script: &str,
    ) -> Result<(), CreateMacroError> {
        Err(CreateMacroError::Internal(anyhow::anyhow!(
            "Cannot create macros with static backend"
        )))
    }

    async fn get_macro(&self, _owner: &str, _name: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn list_macros(&self, _owner: &str) -> Result<Vec<(String, String)>> {
        Ok(vec![])
    }

    async fn delete_macro(&self, _owner: &str, _name: &str) -> Result<bool> {
        Ok(false)
    }

    async fn create_global_macro(&self, _name: &str, _script: &str) -> Result<()> {
        bail!("Cannot create macros with static backend")
    }

    async fn get_global_macro(&self, _name: &str) -> Result<Option<String>> {
        Ok(None)
    }

    async fn list_global_macros(&self) -> Result<Vec<(String, String)>> {
        Ok(vec![])
    }

    async fn delete_global_macro(&self, _name: &str) -> Result<bool> {
        Ok(false)
    }

    async fn resolve_macro(&self, _owner: &str, _command: &str) -> Result<MacroResolution> {
        Ok(MacroResolution::None)
    }

    async fn set_var(&self, _owner: &str, _name: &str, _value: &str) -> Result<(), SetVarError> {
        Err(SetVarError::Internal(anyhow::anyhow!(
            "Cannot set vars with static backend"
        )))
    }

    async fn del_vars(&self, _owner: &str, _keys: &[&str]) -> Result<usize> {
        Ok(0)
    }

    async fn list_vars(&self, _owner: &str) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn clear_vars(&self, _owner: &str) -> Result<()> {
        Ok(())
    }

    async fn set_global_var(&self, _name: &str, _value: &str) -> Result<()> {
        bail!("Cannot set global vars with static backend")
    }

    async fn load_vars(&self, _owner: &str) -> Result<DashMap<String, String>> {
        Ok(DashMap::new())
    }
}

#[async_trait]
impl RunnerBackend for RunnerBackendRedis {
    async fn get_native_command(&self, command: &str) -> Result<Option<Arc<NativeCommand>>> {
        Ok(self.commands.get(command).cloned())
    }

    async fn create_macro(
        &self,
        owner: &str,
        name: &str,
        script: &str,
    ) -> Result<(), CreateMacroError> {
        let key = format!("macros:{owner}");

        let mut t = self.client.create_transaction();
        t.hset(&key, (name, script)).forget();
        t.hlen(&key).queue();

        let len: usize = t.execute().await?;

        if len == 1000 {
            self.client.hdel(key, name).await?;
            Err(CreateMacroError::TooManyMacros)
        } else {
            Ok(())
        }
    }

    async fn get_macro(&self, owner: &str, name: &str) -> Result<Option<String>> {
        Ok(self.client.hget(format!("macros:{owner}"), name).await?)
    }

    async fn list_macros(&self, owner: &str) -> Result<Vec<(String, String)>> {
        Ok(self.client.hgetall(format!("macros:{owner}")).await?)
    }

    async fn delete_macro(&self, owner: &str, name: &str) -> Result<bool> {
        Ok(self.client.hdel(format!("macros:{owner}"), name).await? != 0)
    }

    async fn create_global_macro(&self, name: &str, script: &str) -> Result<()> {
        self.client.hset("macros:global", (name, script)).await?;
        Ok(())
    }

    async fn get_global_macro(&self, name: &str) -> Result<Option<String>> {
        Ok(self.client.hget("macros:global", name).await?)
    }

    async fn list_global_macros(&self) -> Result<Vec<(String, String)>> {
        Ok(self.client.hgetall("macros:global").await?)
    }

    async fn delete_global_macro(&self, name: &str) -> Result<bool> {
        Ok(self.client.hdel("macros:global", name).await? != 0)
    }

    async fn resolve_macro(&self, owner: &str, command: &str) -> Result<MacroResolution> {
        let mut p = self.client.create_pipeline();
        p.hget::<_, _, Option<String>>(format!("macros:{owner}"), command)
            .queue();
        p.hget::<_, _, Option<String>>("macros:global", command)
            .queue();
        let (personal, global): (Option<String>, Option<String>) = p.execute().await?;

        if let Some(script) = personal {
            Ok(MacroResolution::Personal(script))
        } else if let Some(script) = global {
            Ok(MacroResolution::Global(script))
        } else {
            Ok(MacroResolution::None)
        }
    }

    async fn set_var(&self, owner: &str, name: &str, value: &str) -> Result<(), SetVarError> {
        let key = format!("vars:{owner}");

        let mut t = self.client.create_transaction();
        t.hset(&key, (name, value)).forget();
        t.hlen(&key).queue();

        let len: usize = t.execute().await?;

        if len == 1000 {
            self.client.hdel(key, name).await?;
            Err(SetVarError::TooManyVars)
        } else {
            Ok(())
        }
    }

    async fn del_vars(&self, owner: &str, keys: &[&str]) -> Result<usize> {
        Ok(self.client.hdel(format!("vars:{owner}"), keys).await? as usize)
    }

    async fn list_vars(&self, owner: &str) -> Result<Vec<String>> {
        Ok(self.client.hkeys(format!("vars:{owner}")).await?)
    }

    async fn clear_vars(&self, owner: &str) -> Result<()> {
        self.client.del(format!("vars:{owner}")).await?;
        Ok(())
    }

    async fn set_global_var(&self, name: &str, value: &str) -> Result<()> {
        self.client.hset("vars:global", (name, value)).await?;
        Ok(())
    }

    async fn load_vars(&self, owner: &str) -> Result<DashMap<String, String>> {
        let mut pp = self.client.create_pipeline();

        type Pairs = Vec<(String, String)>;

        pp.hgetall::<_, _, _, Pairs>("vars:global").queue();
        pp.hgetall::<_, _, _, Pairs>(format!("vars:{owner}"))
            .queue();

        let (globals, vars): (Pairs, Pairs) = pp.execute().await?;

        Ok(globals.into_iter().chain(vars.into_iter()).collect())
    }
}
