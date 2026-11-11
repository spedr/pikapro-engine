//! Showdown's `getPlayerStreams` (`sim/battle-stream.ts`): one battle
//! stream split into what each audience reads and writes.

use std::collections::VecDeque;

use pikapro::SideId;
use pikapro_protocol::Perspective;

use crate::stream::{BattleStream, Message, StreamError, extract};

/// One of `getPlayerStreams`' streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Every line, exact HP for both sides (Showdown's channel `-1`).
    Omniscient,
    /// Percentages for both sides (channel `0`); read-only.
    Spectator,
    /// A player's view and its requests and errors; writing to it makes
    /// that player's choices.
    Player(SideId),
}

impl Channel {
    pub const ALL: [Channel; 4] =
        [Channel::Omniscient, Channel::Spectator, Channel::Player(SideId::P1), Channel::Player(SideId::P2)];

    fn index(self) -> usize {
        match self {
            Channel::Omniscient => 0,
            Channel::Spectator => 1,
            Channel::Player(SideId::P1) => 2,
            Channel::Player(SideId::P2) => 3,
        }
    }

    fn perspective(self) -> Perspective {
        match self {
            Channel::Omniscient => Perspective::Omniscient,
            Channel::Spectator => Perspective::Spectator,
            Channel::Player(side) => Perspective::Player(side),
        }
    }
}

/// A [`BattleStream`] read and written through `getPlayerStreams`'
/// channels. Each update is pushed to every channel as the lines that
/// channel reads (possibly none: an empty string), each `sideupdate` to
/// its player's channel; `end` and `requesteddata` go to none.
#[derive(Debug)]
pub struct PlayerStreams {
    stream: BattleStream,
    queues: [VecDeque<String>; 4],
}

impl PlayerStreams {
    pub fn new(stream: BattleStream) -> PlayerStreams {
        PlayerStreams { stream, queues: Default::default() }
    }

    /// Writes to a channel: the omniscient channel takes command lines as
    /// they are (`>start`, `>player`, ...); a player's channel takes that
    /// player's choices, each line prefixed with `>pN ` as Showdown's
    /// `data.replace(/(^|\n)/g, '$1>pN ')` does. The spectator channel is
    /// read-only.
    pub fn write(&mut self, channel: Channel, data: &str) -> Result<(), StreamError> {
        let result = match channel {
            Channel::Omniscient => self.stream.write(data),
            Channel::Spectator => Err(StreamError { message: "the spectator stream is read-only".into() }),
            Channel::Player(side) => {
                let prefix = if side == SideId::P1 { ">p1 " } else { ">p2 " };
                let text: Vec<String> = data.split('\n').map(|line| format!("{prefix}{line}")).collect();
                self.stream.write(&text.join("\n"))
            }
        };
        self.pump();
        result
    }

    /// Ends the input.
    pub fn write_end(&mut self) {
        self.stream.write_end();
    }

    /// The next chunk a channel reads.
    pub fn read(&mut self, channel: Channel) -> Option<String> {
        self.pump();
        self.queues[channel.index()].pop_front()
    }

    /// Every chunk a channel has not read yet.
    pub fn drain(&mut self, channel: Channel) -> Vec<String> {
        self.pump();
        self.queues[channel.index()].drain(..).collect()
    }

    /// Whether the channels have ended (the stream has, and every chunk
    /// was routed).
    pub fn is_ended(&self) -> bool {
        self.stream.is_ended()
    }

    pub fn stream(&self) -> &BattleStream {
        &self.stream
    }

    fn pump(&mut self) {
        while let Some(message) = self.stream.read() {
            match message {
                Message::Update(data) => {
                    for channel in Channel::ALL {
                        let lines = extract(&data, channel.perspective());
                        self.queues[channel.index()].push_back(lines.join("\n"));
                    }
                }
                Message::SideUpdate { side, data } => self.queues[Channel::Player(side).index()].push_back(data),
                Message::End(_) | Message::RequestedData(_) | Message::Replay(_) => {}
            }
        }
    }
}
