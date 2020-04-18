use anyhow::{anyhow, ensure, Context, Result};
use async_std::{path::Path, prelude::*, sync::Arc};
use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Executor, Postgres, Row};
use std::{collections::HashMap, convert::TryInto, fmt, str::FromStr};
use url::Url;
use uuid::Uuid;

use crate::{
    scryfall::{
        self, api::ScryfallApi, ScryfallCard, ScryfallCardRow, ScryfallId, ScryfallOracleId,
    },
    tts::RenderedDeck,
    user::{User, UserId},
};

mod loaders;

#[derive(Clone, Debug)]
pub struct ParsedDeck {
    pub title: String,
    pub url: String,
    pub commanders: HashMap<ScryfallOracleId, String>,
    pub main_deck: HashMap<ScryfallOracleId, (String, u8)>,
    pub sideboard: HashMap<ScryfallOracleId, (String, u8)>,
}

impl ParsedDeck {
    pub async fn save(
        self,
        db: &mut impl Executor<Database = Postgres>,
        user: &User,
    ) -> Result<Deck> {
        async fn expand_cards<I>(
            db: &mut impl Executor<Database = Postgres>,
            label: &'static str,
            card_list: I,
        ) -> Result<HashMap<ScryfallId, (ScryfallCard, u8)>>
        where
            I: Iterator<Item = (ScryfallOracleId, (String, u8))> + ExactSizeIterator,
        {
            if card_list.len() == 0 {
                return Ok(HashMap::new());
            }
            debug!("Expanding {} oracle IDs into actual cards", label);
            let mut output = HashMap::with_capacity(card_list.len());
            for (oracle_id, (card_name, oracle_count)) in card_list {
                debug!(
                    "Expanding {}x {} (oracle ID: {})...",
                    oracle_count, card_name, oracle_id
                );
                for (card_id, card, card_count) in
                    scryfall::expand_oracle_id(db, oracle_id, oracle_count).await?
                {
                    debug!(
                        "Expanded {}x {} (from oracle ID: {}) to card {}",
                        card_count, card_name, oracle_id, card_id
                    );
                    output.insert(card_id, (card, card_count));
                }
            }
            Ok(output)
        }
        let commanders = expand_cards(
            db,
            "commanders",
            self.commanders.into_iter().map(|(k, name)| (k, (name, 1))),
        )
        .await?
        .into_iter()
        .map(|(id, (card, count))| {
            if count == 1 {
                Ok((id, card))
            } else {
                Err(anyhow!(
                    "Got multiple copies of a commander card {}: {}",
                    id,
                    card.combined_name()
                ))
            }
        })
        .collect::<Result<HashMap<ScryfallId, ScryfallCard>>>()?;
        let main_deck = expand_cards(db, "main deck", self.main_deck.into_iter()).await?;
        let sideboard = expand_cards(db, "sideboard", self.sideboard.into_iter()).await?;

        debug!("Checking for an existing deck for this user and URL");
        let existing_deck_opt = sqlx::query("SELECT id FROM deck WHERE user_id = $1 AND url = $2;")
            .bind(user.id.as_queryable())
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
                .bind(user.id.as_queryable())
                .bind(self.title.clone())
                .bind(self.url.clone())
                .execute(db)
                .await?;
                ensure!(
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

        async fn insert_deck_entry(
            db: &mut impl Executor<Database = Postgres>,
            deck_id: DeckId,
            card_id: ScryfallId,
            card_count: u8,
            pile: &'static str,
        ) -> Result<()> {
            const INSERT_ENTRY_SQL: &'static str = "\
INSERT INTO deck_entry ( deck_id, card, copies, pile )
VALUES ( $1::uuid, $2::uuid, $3, $4::deck_pile );";
            debug!(
                "Creating row in deck_entry table for deck {} and card {} in pile {}",
                deck_id, card_id, pile,
            );
            sqlx::query(INSERT_ENTRY_SQL)
                .bind(deck_id.as_uuid())
                .bind(card_id.as_uuid())
                .bind(card_count as i16)
                .bind(pile)
                .execute(db)
                .await?;
            Ok(())
        }

        for (card_id, _name) in commanders.iter() {
            insert_deck_entry(db, deck_id, *card_id, 1, "commander").await?;
        }
        for (card_id, (_name, card_count)) in main_deck.iter() {
            insert_deck_entry(db, deck_id, *card_id, *card_count, "main_deck").await?;
        }
        for (card_id, (_name, card_count)) in sideboard.iter() {
            insert_deck_entry(db, deck_id, *card_id, *card_count, "sideboard").await?;
        }

        Ok(Deck {
            id: deck_id,
            user_id: user.id,
            title: self.title,
            url: self.url,
            commanders,
            main_deck,
            sideboard,
            rendered_json: None,
        })
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[repr(transparent)]
pub struct DeckId(Uuid);

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

#[derive(Clone, Debug)]
pub struct Deck {
    pub id: DeckId,
    pub user_id: UserId,
    pub title: String,
    pub url: String,
    pub commanders: HashMap<ScryfallId, ScryfallCard>,
    pub main_deck: HashMap<ScryfallId, (ScryfallCard, u8)>,
    pub sideboard: HashMap<ScryfallId, (ScryfallCard, u8)>,
    pub rendered_json: Option<Value>,
}

struct DeckEntryRow {
    deck_id: DeckId,
    user_id: UserId,
    deck_title: String,
    deck_url: String,
    deck_json: Option<Value>,
    card_id: ScryfallId,
    card_row: ScryfallCardRow,
    copies: u8,
    pile: String,
}

impl sqlx::FromRow<sqlx::postgres::PgRow> for DeckEntryRow {
    fn from_row(row: sqlx::postgres::PgRow) -> Self {
        let deck_json: Option<String> = row.get("deck_json");
        let card_json: String = row.get("card_json");
        // HACK: for some reason this has a 0x01 byte before the actual text
        // column. Strip that off for now.
        let card_json = card_json[1..].to_owned();
        DeckEntryRow {
            deck_id: Uuid::into(row.get("deck_id")),
            user_id: i64::into(row.get("user_id")),
            deck_title: row.get("deck_title"),
            deck_url: row.get("deck_url"),
            deck_json: deck_json.map(|s| serde_json::from_str(&s).unwrap()),
            card_id: Uuid::into(row.get("card_id")),
            card_row: ScryfallCardRow {
                json: card_json,
                updated_at: row.get("card_updated_at"),
            },
            copies: <i16 as TryInto<u8>>::try_into(row.get("copies")).unwrap(),
            pile: row.get("pile"),
        }
    }
}

impl Deck {
    pub async fn render(
        &mut self,
        api: Arc<ScryfallApi>,
        db: &mut impl Executor<Database = Postgres>,
        redis: &mut impl AsyncCommands,
        root: impl AsRef<Path>,
    ) -> Result<RenderedDeck> {
        let rendered = crate::tts::render_deck(api, db, redis, root, self).await?;
        sqlx::query("UPDATE deck SET json = $1::jsonb WHERE id = $2;")
            .bind(serde_json::to_string(&rendered.json_description)?)
            .bind(self.id.as_uuid())
            .execute(db)
            .await?;
        self.rendered_json = Some(rendered.json_description.clone());
        Ok(rendered)
    }

    //     pub async fn get_for_user(
    //         db: &mut impl Executor<Database = Postgres>,
    //         user: UserId,
    //     ) -> Result<Vec<Self>> {
    //         let mut rows = sqlx::query_as(
    //             "\
    // SELECT deck.id as deck_id
    //      , deck.user_id as user_id
    //      , deck.title as deck_title
    //      , deck.url as deck_url
    //      , deck.json::text as deck_json
    //      , deck_entry.card as card_id
    //      , scryfall_card.json as card_json
    //      , scryfall_card.updated_at as card_updated_at
    //      , deck_entry.copies as copies
    //      , deck_entry.is_sideboard as is_sideboard
    // FROM deck_entry
    // INNER JOIN deck
    //   ON (deck.id = deck_entry.deck_id)
    // INNER JOIN scryfall_card
    //   ON (((scryfall_card.json ->> 'id')::uuid) = deck_entry.card)
    // WHERE
    //   deck.user_id = $1
    // ;",
    //         )
    //         .bind(user.as_queryable())
    //         .fetch(db);

    //         let mut decks_by_id: HashMap<DeckId, Deck> = HashMap::new();

    //         while let Some(row) = rows.next().await {
    //             let row: DeckEntryRow = row?;
    //             let deck: &mut Deck = decks_by_id.entry(row.deck_id).or_insert(Deck {
    //                 id: row.deck_id,
    //                 user_id: user,
    //                 title: row.deck_title,
    //                 url: row.deck_url,
    //                 main_deck: HashMap::new(),
    //                 sideboard: HashMap::new(),
    //                 rendered_json: row.deck_json,
    //             });
    //             // let card_id = row.card_id;
    //             let card = row.card_row.try_into()// .with_context(|| {
    //             //     format!(
    //             //         "Getting deck entries for user {} (card ID: {}, deck ID: {})",
    //             //         user, card_id, deck.id,
    //             //     )
    //             // })
    //                 ?;
    //             if row.is_sideboard {
    //                 deck.sideboard.insert(row.card_id, (card, row.copies));
    //             } else {
    //                 deck.main_deck.insert(row.card_id, (card, row.copies));
    //             }
    //         }

    //         let mut decks: Vec<Deck> = decks_by_id.into_iter().map(|(_k, v)| v).collect();
    //         decks.sort_by_key(|d| (d.title.clone(), d.url.clone()));
    //         Ok(decks)
    //     }

    pub async fn get_by_id(
        db: &mut impl Executor<Database = Postgres>,
        id: DeckId,
    ) -> Result<Option<Self>> {
        let mut rows = sqlx::query_as(
            "\
SELECT deck.id as deck_id
     , deck.user_id as user_id
     , deck.title as deck_title
     , deck.url as deck_url
     , deck.json::text as deck_json
     , deck_entry.card as card_id
     , scryfall_card.json as card_json
     , scryfall_card.updated_at as card_updated_at
     , deck_entry.copies as copies
     , deck_entry.pile::text as pile
FROM deck_entry
INNER JOIN deck
  ON (deck.id = deck_entry.deck_id)
INNER JOIN scryfall_card
  ON (((scryfall_card.json ->> 'id')::uuid) = deck_entry.card)
WHERE
  deck.id = $1
;",
        )
        .bind(id.as_uuid())
        .fetch(db);

        let mut deck = None;
        while let Some(row) = rows.next().await {
            let row: DeckEntryRow = row?;
            let card = row.card_row.try_into()?;
            let deck = match deck.as_mut() {
                None => {
                    deck = Some(Deck {
                        id: row.deck_id,
                        user_id: row.user_id,
                        title: row.deck_title,
                        url: row.deck_url,
                        commanders: HashMap::new(),
                        main_deck: HashMap::new(),
                        sideboard: HashMap::new(),
                        rendered_json: row.deck_json,
                    });
                    deck.as_mut().unwrap()
                }
                Some(deck_ref) => deck_ref,
            };
            match row.pile.as_str() {
                "commander" => deck.commanders.insert(row.card_id, card).map(|_| ()),
                "main_deck" => deck
                    .main_deck
                    .insert(row.card_id, (card, row.copies))
                    .map(|_| ()),
                "sideboard" => deck
                    .sideboard
                    .insert(row.card_id, (card, row.copies))
                    .map(|_| ()),
                other => Err(anyhow!("Got unexpected pile value from DB: {:?}", other))?,
            };
        }

        Ok(deck)
    }

    pub async fn delete(self, db: &mut impl Executor<Database = Postgres>) -> Result<()> {
        sqlx::query("DELETE FROM deck WHERE deck.id = $1;")
            .bind(self.id.as_uuid())
            .execute(db)
            .await?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct DeckSummary {
    pub id: DeckId,
    pub user_id: UserId,
    pub title: String,
    pub url: String,
}

impl DeckSummary {
    pub async fn get_for_user(
        db: &mut impl Executor<Database = Postgres>,
        user: UserId,
    ) -> Result<Vec<Self>> {
        let mut rows = sqlx::query("SELECT id, user_id, title, url FROM deck WHERE user_id = $1;")
            .bind(user.as_queryable())
            .fetch(db);
        let mut decks = vec![];
        while let Some(row) = rows.next().await {
            let row = row?;
            decks.push(DeckSummary {
                id: DeckId::from(row.get::<Uuid, _>("id")),
                user_id: UserId::from(row.get::<i64, _>("user_id")),
                title: row.get("title"),
                url: row.get("url"),
            });
        }
        decks.sort_by_key(|d| (d.title.clone(), d.url.clone()));
        Ok(decks)
    }
}

#[async_trait(?Send)]
pub trait DeckLoader: Sized + Sync {
    fn match_url(url: &Url) -> Option<Self>;

    async fn parse_deck(&self, db: &mut impl Executor<Database = Postgres>) -> Result<ParsedDeck>;
}

pub async fn load_deck(
    db: &mut impl Executor<Database = Postgres>,
    user: &User,
    url: &str,
) -> Result<Deck> {
    let url = Url::parse(url).context("Loading a deck")?;
    let parsed: ParsedDeck = if let Some(loader) = loaders::DeckboxLoader::match_url(&url) {
        loader
            .parse_deck(db)
            .await
            .context("Loading deck from Deckbox")?
    } else if let Some(loader) = loaders::TappedOutLoader::match_url(&url) {
        loader
            .parse_deck(db)
            .await
            .context("Loading deck from TappedOut")?
    } else {
        return Err(anyhow!("Invalid URL: {}", url))?;
    };
    let saved = parsed.save(db, user).await?;
    Ok(saved)
}
