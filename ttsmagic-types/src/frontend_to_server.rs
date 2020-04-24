use serde::{Deserialize, Serialize};
use url::Url;

use crate::DeckId;

#[derive(Debug, Deserialize, Serialize)]
pub enum FrontendToServerMessage {
    DeleteDeck { id: DeckId },
    GetDecks,
    RenderDeck { url: Url },
}
