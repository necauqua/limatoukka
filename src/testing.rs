use std::sync::atomic::{AtomicU32, Ordering};

use crate::services::messaging::{Message, PermissionLevel, Sender};

pub trait MessageExt {
    fn permission(self, level: PermissionLevel) -> Self;

    fn sender(self, id: impl Into<String>, login: impl Into<String>) -> Self;

    fn source(self, channel: impl Into<String>) -> Self;
}

impl MessageExt for Message {
    fn permission(mut self, level: PermissionLevel) -> Self {
        self.sender.level = level;
        self
    }

    fn sender(mut self, id: impl Into<String>, login: impl Into<String>) -> Self {
        self.sender.id = id.into();
        self.sender.login = login.into();

        // capitalize ¯\_(ツ)_/¯
        let mut c = self.sender.login.chars();
        self.sender.name = match c.next() {
            None => String::new(),
            Some(ch) => ch.to_uppercase().collect::<String>() + c.as_str(),
        };

        self
    }

    fn source(mut self, channel: impl Into<String>) -> Self {
        self.source_channel = channel.into();
        self
    }
}

pub fn message(text: impl Into<String>) -> Message {
    static MSG_ID: AtomicU32 = AtomicU32::new(1);

    Message {
        id: format!("mock-msg-id-{}", MSG_ID.fetch_add(1, Ordering::Relaxed)),
        source_channel: "mock-channel".into(),
        sender: Sender {
            id: "mock-sender-id".into(),
            name: "mock-name".into(),
            level: PermissionLevel::Viewer,
            login: "mock-login".into(),
        },
        text: text.into(),
        replyable: true,
    }
}
