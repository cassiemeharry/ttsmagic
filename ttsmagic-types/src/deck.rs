use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use url::Url;
use uuid::Uuid;

// use crate::UserId;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[repr(transparent)]
pub struct DeckId(pub Uuid);

impl DeckId {
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for DeckId {
    fn from(uuid: Uuid) -> DeckId {
        DeckId(uuid)
    }
}

impl FromStr for DeckId {
    type Err = <Uuid as FromStr>::Err;
    fn from_str(id: &str) -> Result<Self, Self::Err> {
        let inner = Uuid::from_str(id)?;
        Ok(DeckId(inner))
    }
}

impl fmt::Debug for DeckId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &self.as_uuid())
    }
}

impl fmt::Display for DeckId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.as_uuid().fmt(f)
    }
}

#[derive(Copy, Clone, Default, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct DeckColorIdentity {
    pub black: bool,
    pub blue: bool,
    pub green: bool,
    pub red: bool,
    pub white: bool,
}

impl fmt::Debug for DeckColorIdentity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        const BLACK: &'static str = "\x1b[37;40mblack\x1b[0m";
        const BLUE: &'static str = "\x1b[34mblue\x1b[0m";
        const GREEN: &'static str = "\x1b[32mgreen\x1b[0m";
        const RED: &'static str = "\x1b[31mred\x1b[0m";
        const WHITE: &'static str = "\x1b[30;47mwhite\x1b[0m";
        let colors: &'static [&str] =
            match (self.black, self.blue, self.green, self.red, self.white) {
                (false, false, false, false, false) => &["colorless"],
                (false, false, false, false, true) => &[WHITE],
                (false, false, false, true, false) => &[RED],
                (false, false, false, true, true) => &[RED, WHITE],
                (false, false, true, false, false) => &[GREEN],
                (false, false, true, false, true) => &[GREEN, WHITE],
                (false, false, true, true, false) => &[GREEN, RED],
                (false, false, true, true, true) => &[GREEN, RED, WHITE],
                (false, true, false, false, false) => &[BLUE],
                (false, true, false, false, true) => &[BLUE, WHITE],
                (false, true, false, true, false) => &[BLUE, RED],
                (false, true, false, true, true) => &[BLUE, RED, WHITE],
                (false, true, true, false, false) => &[BLUE, GREEN],
                (false, true, true, false, true) => &[BLUE, GREEN, WHITE],
                (false, true, true, true, false) => &[BLUE, GREEN, RED],
                (false, true, true, true, true) => &[BLUE, GREEN, RED, WHITE],
                (true, false, false, false, false) => &[BLACK],
                (true, false, false, false, true) => &[BLACK, WHITE],
                (true, false, false, true, false) => &[BLACK, RED],
                (true, false, false, true, true) => &[BLACK, RED, WHITE],
                (true, false, true, false, false) => &[BLACK, GREEN],
                (true, false, true, false, true) => &[BLACK, GREEN, WHITE],
                (true, false, true, true, false) => &[BLACK, GREEN, RED],
                (true, false, true, true, true) => &[BLACK, GREEN, RED, WHITE],
                (true, true, false, false, false) => &[BLACK, BLUE],
                (true, true, false, false, true) => &[BLACK, BLUE, WHITE],
                (true, true, false, true, false) => &[BLACK, BLUE, RED],
                (true, true, false, true, true) => &[BLACK, BLUE, RED, WHITE],
                (true, true, true, false, false) => &[BLACK, BLUE, GREEN],
                (true, true, true, false, true) => &[BLACK, BLUE, GREEN, WHITE],
                (true, true, true, true, false) => &[BLACK, BLUE, GREEN, RED],
                (true, true, true, true, true) => &[BLACK, BLUE, GREEN, RED, WHITE],
            };
        write!(f, "DeckColorIdentity {{ {} }}", colors.join(", "))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct Deck {
    pub id: DeckId,
    // pub user_id: UserId,
    pub title: String,
    pub url: Url,
    pub rendered: bool,
    #[serde(default)]
    pub color_identity: DeckColorIdentity,
}
