//! Meetings on our servers (ADR 0030 §6): the shared rooms
//! (`glass_managed::meet::rooms`), reading chat's registered keys and
//! meetings, with their refusals as the console's.

use glass_managed::chat::{ConversationKind, MemberKey};
pub use glass_managed::meet::rooms::Rooms;
use glass_managed::meet::rooms::{Facts, RoomRefusal};

use crate::chat::ChatState;
use crate::state::Refused;

impl Facts for ChatState {
    fn key(&self, id: &str) -> Option<MemberKey> {
        ChatState::key(self, id).cloned()
    }

    fn meeting(&self, id: &str) -> Option<Vec<String>> {
        self.conversations
            .get(id)
            .filter(|stored| stored.conversation.kind == ConversationKind::Meeting)
            .map(|stored| stored.conversation.members.clone())
    }
}

impl From<RoomRefusal> for Refused {
    fn from(r: RoomRefusal) -> Self {
        match r {
            RoomRefusal::Invalid(m) => Refused::Invalid(vec![m]),
            RoomRefusal::Rule(m) => Refused::Rule(m),
        }
    }
}
