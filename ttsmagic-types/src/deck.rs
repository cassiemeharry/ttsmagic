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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct Deck {
    pub id: DeckId,
    // pub user_id: UserId,
    pub title: String,
    pub url: Url,
    pub rendered: bool,
}
