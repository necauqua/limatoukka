use std::{
    any::{Any, TypeId},
    sync::Arc,
};

use dashmap::DashMap;
use messaging::MessagingClient;
use noita::NoitaHandle;
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
pub mod tts;
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
    xdo: XDoClient,
    noita: NoitaHandle,
    status_wall: StatusWall,
    twitch: Twitch,
}

#[derive(Default, Clone)]
pub struct Injector {
    services: Arc<DashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl Injector {
    pub fn add<T: ?Sized + Send + Sync + 'static>(&mut self, service: Arc<T>) {
        self.services.insert(TypeId::of::<T>(), Box::new(service));
    }

    pub fn get_opt<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|s| s.downcast_ref::<Arc<T>>().cloned())
    }

    pub fn get<T: ?Sized + Send + Sync + 'static>(&self) -> Arc<T> {
        self.get_opt()
            .unwrap_or_else(|| panic!("service missing: {}", std::any::type_name::<T>()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait ServiceA: Send + Sync + Any {
        fn meow(&self) -> &'static str;
    }

    struct ServiceAImpl1;
    struct ServiceAImpl2;

    impl ServiceA for ServiceAImpl1 {
        fn meow(&self) -> &'static str {
            "meow"
        }
    }

    impl ServiceA for ServiceAImpl2 {
        fn meow(&self) -> &'static str {
            "woof"
        }
    }

    fn test_command(injector: &Injector) -> &str {
        injector.get::<dyn ServiceA>().meow()
    }

    #[test]
    fn service_1() {
        let mut injector = Injector::default();

        injector.add::<dyn ServiceA>(Arc::new(ServiceAImpl1));

        assert_eq!(test_command(&injector), "meow");
    }

    #[test]
    fn service_2() {
        let mut injector = Injector::default();

        injector.add::<dyn ServiceA>(Arc::new(ServiceAImpl2));

        assert_eq!(test_command(&injector), "woof");
    }
}
