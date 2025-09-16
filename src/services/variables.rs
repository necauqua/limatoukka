use std::borrow::Cow;

use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use rustis::{
    client::{BatchPreparedCommand, Client as ValkeyClient},
    commands::{GenericCommands, HashCommands},
};
use thiserror::Error;

use crate::injector_getter;

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

#[derive(Debug, Clone, Copy)]
pub enum VarType {
    Macro,
    Var,
}

#[derive(Debug, Clone, Copy)]
pub enum VarScope<'o> {
    Personal(&'o str),
    Global,
}

#[derive(Debug, Clone)]
pub enum VarResolution {
    None,
    Personal(String),
    Global(String),
}

#[async_trait]
pub trait VariableStorage: Send + Sync {
    async fn set(
        &self,
        tpe: VarType,
        scope: VarScope<'_>,
        name: &str,
        value: &str,
    ) -> Result<(), SetVarError>;

    async fn get(&self, tpe: VarType, scope: VarScope<'_>, name: &str) -> Result<Option<String>>;

    async fn resolve(&self, tpe: VarType, owner: &str, name: &str) -> Result<VarResolution>;

    async fn delete(&self, tpe: VarType, scope: VarScope<'_>, names: &[&str]) -> Result<usize>;

    async fn list(&self, tpe: VarType, scope: VarScope<'_>) -> Result<Vec<(String, String)>>;

    async fn clear(&self, tpe: VarType, scope: VarScope<'_>) -> Result<()>;

    async fn load_vars(&self, owner: &str) -> Result<DashMap<String, String>>;
}

injector_getter!(VariableStorage::vars);

pub struct VariableStorageRedis {
    client: ValkeyClient,
}

impl VariableStorageRedis {
    pub fn new(client: ValkeyClient) -> Self {
        Self { client }
    }
}

fn key(tpe: VarType, scope: VarScope) -> Cow<'static, str> {
    match (tpe, scope) {
        (VarType::Macro, VarScope::Personal(owner)) => format!("macros:{owner}",).into(),
        (VarType::Macro, VarScope::Global) => "macros:global".into(),
        (VarType::Var, VarScope::Personal(owner)) => format!("vars:{owner}",).into(),
        (VarType::Var, VarScope::Global) => "vars:global".into(),
    }
}

#[async_trait]
impl VariableStorage for VariableStorageRedis {
    async fn set(
        &self,
        tpe: VarType,
        scope: VarScope<'_>,
        name: &str,
        value: &str,
    ) -> Result<(), SetVarError> {
        let key = key(tpe, scope);

        if let VarScope::Personal(_) = scope {
            let mut t = self.client.create_transaction();
            t.hset(&*key, (name, value)).forget();
            t.hlen(&*key).queue();

            let len: usize = t.execute().await?;

            if len == 1000 {
                self.client.hdel(&*key, name).await?;
                return Err(SetVarError::TooManyVars);
            }
        } else {
            self.client.hset(&*key, (name, value)).await?;
        }

        Ok(())
    }

    async fn get(&self, tpe: VarType, scope: VarScope<'_>, name: &str) -> Result<Option<String>> {
        Ok(self.client.hget(&*key(tpe, scope), name).await?)
    }

    async fn resolve(&self, tpe: VarType, owner: &str, name: &str) -> Result<VarResolution> {
        let mut p = self.client.create_pipeline();

        p.hget::<_, _, Option<String>>(&*key(tpe, VarScope::Personal(owner)), name)
            .queue();
        p.hget::<_, _, Option<String>>(&*key(tpe, VarScope::Global), name)
            .queue();

        let (personal, global): (Option<String>, Option<String>) = p.execute().await?;

        if let Some(script) = personal {
            Ok(VarResolution::Personal(script))
        } else if let Some(script) = global {
            Ok(VarResolution::Global(script))
        } else {
            Ok(VarResolution::None)
        }
    }

    async fn delete(&self, tpe: VarType, scope: VarScope<'_>, names: &[&str]) -> Result<usize> {
        Ok(self.client.hdel(&*key(tpe, scope), names).await?)
    }

    async fn list(&self, tpe: VarType, scope: VarScope<'_>) -> Result<Vec<(String, String)>> {
        Ok(self.client.hgetall(&*key(tpe, scope)).await?)
    }

    async fn clear(&self, tpe: VarType, scope: VarScope<'_>) -> Result<()> {
        self.client.del(&*key(tpe, scope)).await?;
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

pub struct VariableStorageMock;

#[async_trait]
impl VariableStorage for VariableStorageMock {
    async fn set(
        &self,
        _tpe: VarType,
        _scope: VarScope<'_>,
        _name: &str,
        _value: &str,
    ) -> Result<(), SetVarError> {
        Err(SetVarError::Internal(anyhow::anyhow!(
            "Cannot create macros or set vars with mock storage"
        )))
    }

    async fn get(
        &self,
        _tpe: VarType,
        _scope: VarScope<'_>,
        _name: &str,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    async fn resolve(&self, _tpe: VarType, _owner: &str, _name: &str) -> Result<VarResolution> {
        Ok(VarResolution::None)
    }

    async fn delete(&self, _tpe: VarType, _scope: VarScope<'_>, _names: &[&str]) -> Result<usize> {
        Ok(0)
    }

    async fn list(&self, _tpe: VarType, _scope: VarScope<'_>) -> Result<Vec<(String, String)>> {
        Ok(vec![])
    }

    async fn clear(&self, _tpe: VarType, _scope: VarScope<'_>) -> Result<()> {
        Ok(())
    }

    // todo make this leak obsolete and remove it
    async fn load_vars(&self, _owner: &str) -> Result<DashMap<String, String>> {
        Ok(DashMap::new())
    }
}
