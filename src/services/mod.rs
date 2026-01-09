use std::{
    any::{Any, TypeId},
    panic::Location,
    sync::Arc,
};

use dashmap::{DashMap, mapref::entry::Entry};

pub mod banishes;
pub mod bets;
pub mod caches;
pub mod charges;
pub mod chat_log;
pub mod display;
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
pub trait Service: Any + Send + Sync + 'static {}

impl<T: ?Sized + Send + Sync + 'static> Service for T {}

// without specialisation we cant implement this for <T> T, sadge, have to hack it into that macro
pub trait ServiceDefault {
    fn new_default() -> Option<Arc<Self>> {
        None
    }
}

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

    pub fn service_opt<T: Service + ?Sized + ServiceDefault>(&self) -> Option<Arc<T>> {
        match self.services.entry(TypeId::of::<T>()) {
            Entry::Occupied(o) => Some(o.get().downcast_ref::<Arc<T>>().unwrap().clone()),
            Entry::Vacant(v) => {
                let d = T::new_default()?;
                v.insert(Box::new(d.clone()));
                Some(d)
            }
        }
    }

    #[track_caller]
    pub fn service<T: Service + ?Sized + ServiceDefault>(&self) -> Arc<T> {
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

#[macro_export]
macro_rules! injector_getter {
    (__ext $service:ident::$name:ident) => {
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
    ($service:ident::$name:ident) => {
        $crate::injector_getter!(__ext $service::$name);

        impl $crate::services::ServiceDefault for dyn $service {}
    };
    ($service:ident::$name:ident { $($t:tt)* }) => {
        $crate::injector_getter!(__ext $service::$name);

        impl $crate::services::ServiceDefault for dyn $service {
            fn new_default() -> Option<::std::sync::Arc<dyn $service>> {
                Some(::std::sync::Arc::new({ $($t)* }))
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

    impl ServiceDefault for dyn ServiceA {
        fn new_default() -> Option<Arc<dyn ServiceA>> {
            Some(Arc::new(ServiceAImpl2))
        }
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

    #[test]
    fn service_default() {
        let injector = Injector::default();

        assert_eq!(test_command(&injector), "woof");
    }
}
