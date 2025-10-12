use std::{
    any::{Any, TypeId},
    panic::Location,
    sync::Arc,
};

use dashmap::DashMap;
use tokio::sync::mpsc::Receiver;

pub mod banishes;
pub mod bets;
pub mod caches;
pub mod charges;
pub mod chat_log;
pub mod gates;
pub mod ipc;
pub mod messaging;
pub mod music;
pub mod noita;
pub mod sounds;
pub mod stats;
pub mod status_wall;
pub mod storage;
pub mod tts;
pub mod twitch;
pub mod variables;

#[derive(Default, Clone)]
pub struct Injector {
    services: Arc<DashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

// maybe do the sealed thing
pub trait Service: Any + Send + Sync + 'static {
    fn events(&self) -> Option<Receiver<Box<dyn Event>>> {
        None
    }
}

impl<T: ?Sized + Send + Sync + 'static> Service for T {}

impl Injector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with<T: Service + ?Sized>(mut self, service: Arc<T>) -> Self {
        self.add(service);
        self
    }

    pub fn add<T: Service + ?Sized>(&mut self, service: Arc<T>) {
        self.services.insert(TypeId::of::<T>(), Box::new(service));
    }

    pub fn service_opt<T: Service + ?Sized>(&self) -> Option<Arc<T>> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|s| s.downcast_ref::<Arc<T>>().cloned())
    }

    #[track_caller]
    pub fn service<T: Service + ?Sized>(&self) -> Arc<T> {
        match self.service_opt() {
            Some(s) => s,
            None => panic!(
                "service missing: {} at {}",
                std::any::type_name::<T>(),
                Location::caller()
            ),
        }
    }
}

pub trait Event: Send + Sync + 'static {}

impl<T: Send + Sync + 'static> Event for T {}

#[macro_export]
macro_rules! injector_getter {
    ($service:ident::$name:ident) => {
        paste::paste! {
            pub trait [<$service Ext>] {
                #[doc = concat!("Get the [`", stringify!($service), "`] service from the injector.")]
                fn $name(&self) -> ::std::sync::Arc<dyn $service>;
            }

            impl [<$service Ext>] for $crate::services::Injector {
                #[inline]
                #[track_caller]
                fn $name(&self) -> ::std::sync::Arc<dyn $service> {
                    self.service()
                }
            }
        }
    };
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
        injector.service::<dyn ServiceA>().meow()
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
