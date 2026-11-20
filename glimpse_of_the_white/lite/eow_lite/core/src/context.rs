use std::collections::VecDeque;

use serde::Serialize;

const MAX_HISTORY_ROUNDS: usize = 10;

#[derive(Serialize)]
pub struct Message {
    role: &'static str,
    content: String,
}

impl Message {

    fn new(role: &'static str, content: &str) -> Self {
        Self {
            role,
            content: content.to_owned(),
        }
    }

}

struct Round {
    inbound: String,
    outbound: String,
}

#[derive(Default)]
pub struct History {
    rounds: VecDeque<Round>,
}

impl History {

    pub fn messages(&self, prompt: &str, inbound: &str) -> Vec<Message> {
        let mut messages: Vec<Message> = Vec::with_capacity(2 + self.rounds.len() * 2);
        messages.push(Message::new("system", prompt));
        for round in &self.rounds {
            messages.push(Message::new("user", &round.inbound));
            messages.push(Message::new("assistant", &round.outbound));
        }
        messages.push(Message::new("user", inbound));
        messages
    }

    pub fn commit(&mut self, inbound: String, outbound: String) {
        self.rounds.push_back(Round { inbound, outbound });
        while self.rounds.len() > MAX_HISTORY_ROUNDS {
            self.rounds.pop_front();
        }
    }

}
