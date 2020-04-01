use async_std::{path::Path, sync::Arc};
use async_trait::async_trait;
use failure::{Error, ResultExt};
use serde_json::Value;
use sqlx::{Executor, Postgres, Row};
use std::{collections::HashMap, fmt, str::FromStr};
use url::Url;
use uuid::Uuid;

use crate::{
    scryfall::{api::ScryfallApi, ScryfallCard, ScryfallId},
    tts::RenderedDeck,
    user::UserId,
};

mod loaders;

#[derive(Clone, Debug)]
pub struct ParsedDeck {
    pub title: String,
    pub url: String,
    pub main_deck: HashMap<ScryfallId, (ScryfallCard, u8)>,
    pub sideboard: HashMap<ScryfallId, (ScryfallCard, u8)>,
}

impl ParsedDeck {
    pub async fn save(
        self,
        db: &mut impl Executor<Database = Postgres>,
        user_id: UserId,
    ) -> Result<Deck, Error> {
        debug!("Checking for an existing deck for this user and URL");
        let existing_deck_opt = sqlx::query("SELECT id FROM deck WHERE user_id = $1 AND url = $2;")
            .bind(user_id.as_queryable())
            .bind(&self.url)
            .fetch_optional(db)
            .await?;
        let deck_id = match existing_deck_opt {
            Some(row) => {
                debug!("Got row with {} values", row.len());
                let deck_id: Uuid = row.get("id");
                debug!("Updating deck {}", deck_id);
                sqlx::query("UPDATE deck SET title = $1, json = $2::jsonb WHERE id = $3;")
                    .bind(&self.title)
                    .bind(None::<&str>)
                    .bind(deck_id)
                    .execute(db)
                    .await?;
                DeckId(deck_id)
            }
            None => {
                let deck_id = DeckId(Uuid::new_v4());
                debug!("Creating row in deck table with ID {}", deck_id);
                let inserted = sqlx::query(
                    "\
INSERT INTO deck ( id, user_id, title, url )
VALUES ( $1, $2, $3, $4 )
ON CONFLICT (id) DO UPDATE SET user_id = $2, title = $3, url = $4;",
                )
                .bind(deck_id.as_uuid())
                .bind(user_id.as_queryable())
                .bind(self.title.clone())
                .bind(self.url.clone())
                .execute(db)
                .await?;
                failure::ensure!(
                    inserted == 1,
                    "Problem inserting deck row. Expected 1 row modified, saw {} instead",
                    inserted
                );
                deck_id
            }
        };
        sqlx::query("DELETE FROM deck_entry WHERE deck_id = $1")
            .bind(deck_id.as_uuid())
            .execute(db)
            .await?;
        const INSERT_ENTRY_SQL: &'static str = "\
INSERT INTO deck_entry ( deck_id, card, copies, is_sideboard )
VALUES ( $1::uuid, $2::uuid, $3, $4 );";
        for (card_id, (_, count)) in self.main_deck.iter() {
            debug!(
                "Creating row in deck_entry table for deck {} and card {}",
                deck_id, card_id
            );
            sqlx::query(INSERT_ENTRY_SQL)
                .bind(deck_id.as_uuid())
                .bind(card_id.as_uuid())
                .bind(*count as i16)
                .bind(false)
                .execute(db)
                .await?;
        }
        for (card_id, (_, count)) in self.sideboard.iter() {
            debug!(
                "Creating row in deck_entry table for deck {} and sideboard card {}",
                deck_id, card_id
            );
            sqlx::query(INSERT_ENTRY_SQL)
                .bind(deck_id.0)
                .bind(card_id.as_uuid())
                .bind(*count as i16)
                .bind(true)
                .execute(db)
                .await?;
        }
        Ok(Deck {
            id: deck_id,
            user_id,
            title: self.title,
            url: self.url,
            main_deck: self.main_deck,
            sideboard: self.sideboard,
            rendered_json: None,
        })
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct DeckId(Uuid);

impl DeckId {
    pub fn as_uuid(self) -> Uuid {
        self.0
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

#[derive(Clone, Debug)]
pub struct Deck {
    pub id: DeckId,
    pub user_id: UserId,
    pub title: String,
    pub url: String,
    pub main_deck: HashMap<ScryfallId, (ScryfallCard, u8)>,
    pub sideboard: HashMap<ScryfallId, (ScryfallCard, u8)>,
    pub rendered_json: Option<Value>,
}

impl Deck {
    pub async fn render(
        &mut self,
        api: Arc<ScryfallApi>,
        db: &mut impl Executor<Database = Postgres>,
        root: impl AsRef<Path>,
    ) -> Result<RenderedDeck, Error> {
        let rendered = crate::tts::render_deck(api, db, root, self).await?;
        sqlx::query("UPDATE deck SET json = $1::jsonb WHERE id = $2;")
            .bind(serde_json::to_string(&rendered.json_description)?)
            .bind(self.id.as_uuid())
            .execute(db)
            .await?;
        self.rendered_json = Some(rendered.json_description.clone());
        Ok(rendered)
    }
}

#[async_trait(?Send)]
pub trait DeckLoader: Sync {
    type UrlInfo;

    fn match_url(url: &Url) -> Option<Self::UrlInfo>;

    async fn parse_deck(
        api: &ScryfallApi,
        db: &mut impl Executor<Database = Postgres>,
        url_info: Self::UrlInfo,
    ) -> Result<ParsedDeck, Error>;
}

pub async fn load_deck(
    api: &ScryfallApi,
    db: &mut impl Executor<Database = Postgres>,
    user_id: UserId,
    url: &str,
) -> Result<Deck, Error> {
    let url = Url::parse(url).context("Loading a deck")?;
    let parsed: ParsedDeck = if let Some(m) = loaders::DeckboxLoader::match_url(&url) {
        loaders::DeckboxLoader::parse_deck(api, db, m)
            .await
            .context("Loading deck from Deckbox")?
    } else {
        return Err(failure::format_err!("Invalid URL: {}", url))?;
    };
    let saved = parsed.save(db, user_id).await?;
    Ok(saved)
}
