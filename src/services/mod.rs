use std::{
    any::{Any, TypeId},
    sync::Arc,
};

use dashmap::DashMap;

pub mod charges;
pub mod chat_log;
pub mod gates;
pub mod messaging;
pub mod noita;
pub mod sounds;
pub mod status_wall;
pub mod storage;
pub mod tts;
pub mod twitch;

#[derive(Default, Clone)]
pub struct Injector {
    services: Arc<DashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl Injector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with<T: ?Sized + Send + Sync + 'static>(mut self, service: Arc<T>) -> Self {
        self.add(service);
        self
    }

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
