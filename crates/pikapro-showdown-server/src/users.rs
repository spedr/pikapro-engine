//! Users and connections (Showdown's `server/users.ts`, without accounts).

use std::collections::HashMap;

/// A websocket connection.
pub type ConnId = u64;
/// A user, stable across renames.
pub type UserKey = u64;
/// Every user by key.
pub type Users = HashMap<UserKey, User>;

/// The settings `|updateuser|` carries (Showdown's defaults).
pub const SETTINGS: &str = concat!(
    r#"{"blockChallenges":false,"blockPMs":false,"ignoreTickets":false,"hideBattlesFromTrainerCard":false,"#,
    r#""blockInvites":false,"doNotDisturb":false,"blockFriendRequests":false,"allowFriendNotifications":false,"#,
    r#""displayBattlesToFriends":false,"hideLogins":false,"hiddenNextBattle":false,"inviteOnlyNextBattle":false,"#,
    r#""language":null}"#
);

/// Showdown's default `|customgroups|` list.
pub const CUSTOM_GROUPS: &str = concat!(
    r#"|customgroups|[{"symbol":"~","name":"Administrator","type":"leadership"},"#,
    r##"{"symbol":"#","name":"Room Owner","type":"leadership"},"##,
    r#"{"symbol":"★","name":"Host","type":"leadership"},{"symbol":"@","name":"Moderator","type":"staff"},"#,
    r#"{"symbol":"%","name":"Driver","type":"staff"},{"symbol":"*","name":"Bot","type":"normal"},"#,
    r#"{"symbol":"☆","name":"Player","type":"normal"},{"symbol":"+","name":"Voice","type":"normal"},"#,
    r#"{"symbol":"^","name":"Prize Winner","type":"normal"},{"symbol":"whitelist","name":"Whitelist","type":"normal"},"#,
    r#"{"symbol":" ","name":null,"type":"normal"},{"symbol":"‽","name":"Locked","type":"punishment"},"#,
    r#"{"symbol":"!","name":"Muted","type":"punishment"}]"#
);

/// A user.
#[derive(Clone, Debug)]
pub struct User {
    pub key: UserKey,
    /// `toID(name)`.
    pub id: String,
    pub name: String,
    /// Chose a name (`/trn`); guests have not.
    pub named: bool,
    pub avatar: String,
    pub conns: Vec<ConnId>,
    /// The packed team `/utm` set (`battleSettings.team`).
    pub team: String,
    /// Battle rooms the user plays in, in the order they started.
    pub games: Vec<String>,
    /// Rooms the user is in.
    pub rooms: Vec<String>,
    /// The throttle's budget, in thousandths of a line, as of
    /// `budget_at` (ms); `None` until the first line.
    pub budget: Option<(u64, u64)>,
}

impl User {
    /// `getIdentity`: the global group symbol (a space) and the name.
    pub fn identity(&self) -> String {
        format!(" {}", self.name)
    }

    /// `getUpdateuserText`.
    pub fn updateuser(&self) -> String {
        format!("|updateuser| {}|{}|{}|{}", self.name, u8::from(self.named), self.avatar, SETTINGS)
    }
}
