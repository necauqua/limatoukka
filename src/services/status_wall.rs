use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use indexmap::IndexMap;
use maud::{PreEscaped, html};
use serde::{Deserialize, Serialize};

use crate::{
    injector_getter,
    services::{Service, display::DisplayHandle},
};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct EntryKey(usize);

pub trait StatusService: Service {
    fn new_key(&self) -> EntryKey;

    fn set(&self, key: EntryKey, text: String) -> Option<String>;

    fn remove(&self, key: EntryKey) -> Option<String>;
}

injector_getter!(StatusService::status { MockStatusWall::default() });

pub struct EntryGuard {
    key: EntryKey,
    wall: Arc<dyn StatusService>,
}

impl EntryGuard {
    pub fn key(&self) -> EntryKey {
        self.key
    }

    pub fn set(&self, text: String) -> Option<String> {
        self.wall.set(self.key, text)
    }
}

impl Drop for EntryGuard {
    fn drop(&mut self) {
        self.wall.remove(self.key);
    }
}

impl dyn StatusService {
    pub fn allocate(self: &Arc<Self>) -> EntryGuard {
        EntryGuard {
            key: self.new_key(),
            wall: self.clone(),
        }
    }

    pub async fn push(self: &Arc<Self>, text: impl Into<String>) -> EntryGuard {
        let entry = self.allocate();
        entry.set(text.into());
        entry
    }
}

pub struct StatusWall {
    entries: Mutex<IndexMap<EntryKey, String>>,
    counter: AtomicUsize,
    display: DisplayHandle,
}

impl StatusWall {
    pub fn new(display: DisplayHandle) -> Self {
        Self {
            entries: Default::default(),
            counter: AtomicUsize::new(0),
            display,
        }
    }

    fn update<R>(&self, f: impl FnOnce(&mut IndexMap<EntryKey, String>) -> R) -> R {
        let (r, text) = {
            let mut entries = self.entries.lock().unwrap();
            let r = f(&mut entries);
            let text = entries
                .iter()
                .fold(String::new(), |acc, (_, entry)| acc + entry + "\n");
            (r, text)
        };
        self.display.set(html! {
            div style="text-align: right;" {
                (PreEscaped(&text))
            }
        });
        r
    }
}

impl StatusService for StatusWall {
    fn new_key(&self) -> EntryKey {
        EntryKey(self.counter.fetch_add(1, Ordering::Relaxed))
    }

    fn set(&self, key: EntryKey, text: String) -> Option<String> {
        self.update(|entries| entries.insert(key, text))
    }

    fn remove(&self, key: EntryKey) -> Option<String> {
        self.update(|entries| entries.shift_remove(&key))
    }
}

#[derive(Default)]
pub struct MockStatusWall(AtomicUsize);

impl StatusService for MockStatusWall {
    fn new_key(&self) -> EntryKey {
        EntryKey(self.0.fetch_add(1, Ordering::Relaxed))
    }

    fn set(&self, key: EntryKey, text: String) -> Option<String> {
        tracing::info!(key = key.0, %text, "set status");
        None
    }

    fn remove(&self, key: EntryKey) -> Option<String> {
        tracing::info!(key = key.0, "remove status");
        None
    }
}
