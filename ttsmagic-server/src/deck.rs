use anyhow::{anyhow, ensure, Context, Result};
use async_std::{prelude::*, sync::Arc};
use futures::future::LocalBoxFuture;
use redis::AsyncCommands;
use serde_json::Value;
use sqlx::{Executor, PgConnection, Postgres, Row};
use std::{collections::HashMap, convert::TryInto, fmt};
use ttsmagic_types::{server_to_frontend as s2f, DeckColorIdentity, DeckId, UserId};
use url::Url;
use uuid::Uuid;

use crate::{
    notify::notify_user,
    scryfall::{
        self, api::ScryfallApi, ScryfallCard, ScryfallCardRow, ScryfallId, ScryfallOracleId,
    },
    tts::RenderedDeck,
    user::User,
    // utils::sqlx::PgArray1D,
};

mod loaders;

async fn expand_cards<I>(
    db: &mut PgConnection,
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
            scryfall::expand_oracle_id(&mut *db, oracle_id, oracle_count).await?
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

async fn insert_deck_entry(
    db: impl sqlx::Executor<'_, Database = Postgres>,
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

#[derive(Clone, Debug)]
pub struct UnparsedDeck {
    pub id: DeckId,
    pub user_id: UserId,
    pub url: Url,
    pub title: String,
}

impl fmt::Display for UnparsedDeck {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.title.is_empty() || self.title.as_str() == self.url.as_str() {
            write!(f, "unparsed deck {:?} from URL {}", self.id, self.url)
        } else {
            write!(
                f,
                "unparsed deck {:?} ({:?}) from URL {}",
                self.title, self.id, self.url
            )
        }
    }
}

impl UnparsedDeck {
    async fn save(
        db: &mut PgConnection,
        redis: &mut impl AsyncCommands,
        user: &User,
        url: Url,
    ) -> Result<Self> {
        debug!("Checking for an existing deck for this user and URL");
        let url_str = format!("{}", url);
        let existing_deck_opt =
            sqlx::query("SELECT id, title FROM deck WHERE user_id = $1 AND url = $2;")
                .bind(user.id.as_queryable())
                .bind(&url_str)
                .fetch_optional(&mut *db)
                .await?;
        let (deck_id, title) = match existing_deck_opt {
            Some(row) => {
                debug!("Got row with {} values", row.len());
                let deck_id: Uuid = row.get("id");
                let title = row.get("title");
                debug!("Updating deck {}", deck_id);
                sqlx::query("UPDATE deck SET json = $1::jsonb WHERE id = $2;")
                    .bind(None::<&str>)
                    .bind(deck_id)
                    .execute(&mut *db)
                    .await?;
                (DeckId(deck_id), title)
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
                .bind(&url_str)
                .bind(&url_str)
                .execute(&mut *db)
                .await?;
                ensure!(
                    inserted.rows_affected() == 1,
                    "Problem inserting deck row. Expected 1 row modified, saw {} instead",
                    inserted.rows_affected()
                );
                (deck_id, url_str)
            }
        };
        sqlx::query("DELETE FROM deck_entry WHERE deck_id = $1")
            .bind(deck_id.as_uuid())
            .execute(&mut *db)
            .await?;

        notify_user(
            redis,
            user.id,
            s2f::Notification::DeckParseStarted {
                deck_id: deck_id,
                title: title.clone(),
                url: url.clone(),
            },
        )
        .await?;

        Ok(UnparsedDeck {
            id: deck_id,
            user_id: user.id,
            url,
            title,
        })
    }

    pub async fn save_cards<R>(
        self,
        db: &mut PgConnection,
        redis: &mut R,
        title: String,
        commanders: HashMap<ScryfallOracleId, String>,
        main_deck: HashMap<ScryfallOracleId, (String, u8)>,
        sideboard: HashMap<ScryfallOracleId, (String, u8)>,
    ) -> Result<Deck>
    where
        R: AsyncCommands,
    {
        debug!("Saving cards for deck {:?}", title);
        sqlx::query("UPDATE deck SET title = $1 WHERE id = $2;")
            .bind(&title)
            .bind(self.id.as_uuid())
            .execute(&mut *db)
            .await?;
        sqlx::query("DELETE FROM deck_entry WHERE deck_id = $1;")
            .bind(self.id.as_uuid())
            .execute(&mut *db)
            .await?;
        let commanders_len = commanders.len();
        let commanders_iter = commanders.into_iter().map(|(k, name)| (k, (name, 1)));
        let mut commanders = HashMap::with_capacity(commanders_len);
        for (card_id, (name, count)) in
            expand_cards(&mut *db, "commanders", commanders_iter).await?
        {
            assert_eq!(count, 1);
            let prev = commanders.insert(card_id, name);
            assert!(prev.is_none());
        }
        let main_deck = expand_cards(&mut *db, "main deck", main_deck.into_iter()).await?;
        let sideboard = expand_cards(&mut *db, "sideboard", sideboard.into_iter()).await?;

        for (card_id, _) in commanders.iter() {
            insert_deck_entry(&mut *db, self.id, *card_id, 1, "commander").await?;
        }
        for (card_id, (_, card_count)) in main_deck.iter() {
            insert_deck_entry(&mut *db, self.id, *card_id, *card_count, "main_deck").await?;
        }
        for (card_id, (_, card_count)) in sideboard.iter() {
            insert_deck_entry(&mut *db, self.id, *card_id, *card_count, "sideboard").await?;
        }

        let color_identity = {
            let mut rows = sqlx::query(
                "\
SELECT DISTINCT jsonb_array_elements_text(sc.json -> 'color_identity') AS color_identity
FROM deck_entry
INNER JOIN scryfall_card sc
  ON ((sc.json ->> 'id')::uuid = deck_entry.card)
WHERE deck_id = $1;",
            )
            .bind(self.id.as_uuid())
            .fetch(&mut *db);
            let mut ci = DeckColorIdentity::default();
            while let Some(row) = rows.next().await {
                let row = row?;
                let color = row.get::<String, _>("color_identity");
                match color.as_str() {
                    "B" => ci.black = true,
                    "U" => ci.blue = true,
                    "G" => ci.green = true,
                    "R" => ci.red = true,
                    "W" => ci.white = true,
                    other => warn!(
                        "Got an unexpected color when parsing deck {:?}'s color identity: {:?}",
                        self.id, other
                    ),
                }
            }
            ci
        };
        notify_user(
            redis,
            self.user_id,
            s2f::Notification::DeckParsed {
                deck_id: self.id,
                title: title.clone(),
                url: self.url.clone(),
                color_identity,
            },
        )
        .await?;

        Ok(Deck {
            id: self.id,
            user_id: self.user_id,
            title: title,
            url: self.url,
            commanders,
            main_deck,
            sideboard,
            rendered_json: None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Deck {
    pub id: DeckId,
    pub user_id: UserId,
    pub title: String,
    pub url: Url,
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

impl sqlx::FromRow<'_, sqlx::postgres::PgRow> for DeckEntryRow {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        let deck_json: Option<String> = row.try_get("deck_json")?;
        let card_json: String = row.try_get("card_json")?;
        // HACK: for some reason this has a 0x01 byte before the actual text
        // column. Strip that off for now.
        let card_json = card_json[1..].to_owned();
        let row = DeckEntryRow {
            deck_id: Uuid::into(row.try_get("deck_id")?),
            user_id: i64::into(row.try_get("user_id")?),
            deck_title: row.try_get("deck_title")?,
            deck_url: row.try_get("deck_url")?,
            deck_json: deck_json.map(|s| serde_json::from_str(&s).unwrap()),
            card_id: Uuid::into(row.try_get("card_id")?),
            card_row: ScryfallCardRow {
                json: card_json,
                updated_at: row.try_get("card_updated_at")?,
            },
            copies: <i16 as TryInto<u8>>::try_into(row.try_get("copies")?).unwrap(),
            pile: row.try_get("pile")?,
        };
        Ok(row)
    }
}

impl Deck {
    pub async fn render<R>(
        &mut self,
        api: Arc<ScryfallApi>,
        db: &mut PgConnection,
        redis: &mut R,
    ) -> Result<RenderedDeck>
    where
        R: AsyncCommands,
    {
        let rendered = crate::tts::render_deck(api, &mut *db, redis, self).await?;
        sqlx::query("UPDATE deck SET json = $1::jsonb WHERE id = $2;")
            .bind(serde_json::to_string(&rendered.json_description)?)
            .bind(self.id.as_uuid())
            .execute(&mut *db)
            .await?;
        self.rendered_json = Some(rendered.json_description.clone());
        Ok(rendered)
    }

    pub async fn get_by_id(
        db: impl sqlx::Executor<'_, Database = Postgres>,
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
                        url: Url::parse(&row.deck_url)?,
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

    pub async fn delete(
        self,
        db: impl Executor<'_, Database = Postgres>,
        redis: &mut impl AsyncCommands,
    ) -> Result<()> {
        sqlx::query("DELETE FROM deck WHERE deck.id = $1;")
            .bind(self.id.as_uuid())
            .execute(db)
            .await?;

        notify_user(
            redis,
            self.user_id,
            s2f::Notification::DeckDeleted { deck_id: self.id },
        )
        .await?;

        Ok(())
    }
}

pub async fn get_decks_for_user(
    db: impl Executor<'_, Database = Postgres>,
    user: UserId,
) -> Result<Vec<ttsmagic_types::Deck>> {
    let mut rows = sqlx::query(
        "\
SELECT id, user_id, title, url, (json IS NOT NULL) as rendered
  , array(
      SELECT DISTINCT jsonb_array_elements_text(sc.json -> 'color_identity') AS color_identity
      FROM deck_entry
      INNER JOIN scryfall_card sc
        ON ((sc.json ->> 'id')::uuid = deck_entry.card) WHERE deck_entry.deck_id = deck.id
    ) :: text[] AS color_identity
FROM deck
WHERE user_id = $1;",
    )
    .bind(user.as_queryable())
    .fetch(db);
    let mut decks = vec![];
    while let Some(row) = rows.next().await {
        let row = row?;
        let deck_id = row.get::<Uuid, _>("id");
        let url: String = row.get("url");
        let color_identity = {
            let raw_identity = row.get::<Vec<String>, _>("color_identity");
            let mut ci = DeckColorIdentity::default();
            for color in raw_identity {
                match color.as_str() {
                    "B" => ci.black = true,
                    "U" => ci.blue = true,
                    "G" => ci.green = true,
                    "R" => ci.red = true,
                    "W" => ci.white = true,
                    other => warn!(
                        "Got an unexpected color value when parsing a deck's color identity: {:?}",
                        other
                    ),
                }
            }
            ci
        };
        decks.push(ttsmagic_types::Deck {
            id: DeckId::from(deck_id),
            // user_id: UserId::from(row.get::<i64, _>("user_id")),
            title: row.get("title"),
            url: Url::parse(&url)?,
            rendered: row.get("rendered"),
            color_identity,
        });
    }
    decks.sort_by_key(|d| (d.title.clone(), d.url.clone()));
    Ok(decks)
}

pub trait DeckParser<R>
where
    R: AsyncCommands,
{
    fn name(&self) -> &'static str;

    fn canonical_deck_url(&self) -> Url;

    fn parse_deck<'a>(
        &'a self,
        db: &'a mut PgConnection,
        redis: &'a mut R,
        unparsed: UnparsedDeck,
    ) -> LocalBoxFuture<'a, Result<Deck>>;
}

pub trait DeckMatcher: Sized {
    fn match_url(url: &Url) -> Option<Self>;
}

pub trait DeckLoader<R>: DeckMatcher + DeckParser<R>
where
    R: AsyncCommands,
{
}

impl<R, T> DeckLoader<R> for T
where
    T: DeckMatcher + DeckParser<R>,
    R: AsyncCommands,
{
}

pub fn find_loader<R>(url: Url) -> Result<Box<dyn DeckParser<R>>, Vec<&'static str>>
where
    R: AsyncCommands,
{
    let mut tried = vec![];
    macro_rules! try_loader {
        ($label:literal => $loader:ident) => {
            match loaders::$loader::match_url(&url) {
                Some(l) => return Ok(Box::new(l)),
                None => tried.push($label),
            }
        };
    }
    try_loader!("Deckbox" => DeckboxLoader);
    try_loader!("TappedOut" => TappedOutLoader);
    try_loader!("Archidekt" => ArchidektLoader);
    Err(tried)
}

pub async fn load_deck<R>(
    db: &mut PgConnection,
    redis: &mut R,
    user: &User,
    url: Url,
) -> Result<Deck>
where
    R: AsyncCommands,
{
    let loader = match find_loader(url.clone()) {
        Ok(l) => l,
        Err(tried) => {
            return Err(anyhow!(
                "Unknown deck site. We tried to load from the following deck sites: {}",
                tried.join(", "),
            ))
            .with_context(|| format!("Failed to load deck from URL {}", url));
        }
    };
    let canon_url = loader.canonical_deck_url();
    let unparsed_future = UnparsedDeck::save(&mut *db, redis, user, canon_url);
    let unparsed = unparsed_future
        .await
        .with_context(|| format!("Failed to save {} deck with URL {}", loader.name(), url))?;
    debug!("UnparsedDeck saved: {:?}", unparsed);
    let load_future = loader.parse_deck(db, redis, unparsed.clone());
    let deck = load_future
        .await
        .with_context(|| format!("Failed to load contents of {}", unparsed))?;
    drop(loader);
    Ok(deck)
}
