use std::sync::Arc;

use chat_log::ChatLog;
use messaging::MessagingClient;
use noita::NoitaHandle;
use sounds::Sounds;
use status_wall::StatusWall;
use storage::Storage;
use twitch::Twitch;
use xdo::XDoClient;

pub mod chat_log;
pub mod messaging;
pub mod noita;
pub mod sounds;
pub mod status_wall;
pub mod storage;
pub mod twitch;
pub mod xdo;

macro_rules! services {
    ($($name:ident : $type:ty),* $(,)?) => {
        #[derive(Clone)]
        pub struct Services {
            $(
                $name: Option<Arc<$type>>,
            )*
        }

        impl Services {
            #[allow(clippy::too_many_arguments)]
            pub fn new($($name: $type),*) -> Self {
                Self {
                    $($name: Some(Arc::new($name)),)*
                }
            }

            #[cfg(test)]
            pub fn mock() -> Self {
                Self {
                    $($name: None,)*
                }
            }

            $(
                pub fn $name(&self) -> &$type {
                    self.$name.as_ref().unwrap()
                }

                paste::paste! {
                    #[cfg(test)]
                    pub fn [< with_ $name >](self, $name: $type) -> Self {
                        Self {
                            $name: Some(Arc::new($name)),
                            ..self
                        }
                    }
                }
            )*
        }
    };
}

services! {
    messaging: MessagingClient,
    storage: Storage,
    chat_log: ChatLog,
    xdo: XDoClient,
    noita: NoitaHandle,
    status_wall: StatusWall,
    twitch: Twitch,
    sounds: Sounds,
}
