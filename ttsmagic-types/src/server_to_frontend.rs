use serde::{Deserialize, Serialize};
use std::num::NonZeroU16;
use url::Url;

use crate::{Deck, DeckColorIdentity, DeckId};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Error {
    pub user_message: String,
    pub details: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum RenderProgress {
    Waiting {
        queue_length: NonZeroU16,
    },
    RenderingImages {
        rendered_cards: u16,
        total_cards: NonZeroU16,
    },
    SavingPages {
        saved_pages: u16,
        total_pages: NonZeroU16,
    },
    Rendered,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Notification {
    DeckDeleted {
        deck_id: DeckId,
    },
    DeckParseStarted {
        deck_id: DeckId,
        title: String,
        url: Url,
    },
    DeckParsed {
        deck_id: DeckId,
        title: String,
        url: Url,
        color_identity: DeckColorIdentity,
    },
    Error(Error),
    RenderProgress {
        deck_id: DeckId,
        progress: RenderProgress,
    },
}

#[derive(Debug, Deserialize, Serialize)]
pub enum ServerToFrontendMessage {
    DeckList { decks: Vec<Deck> },
    FatalError(Error),
    Notification(Notification),
}
