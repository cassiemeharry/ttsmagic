use async_std::{path::Path, prelude::*};
use chrono::{prelude::*, Duration};
use failure::{format_err, Error, ResultExt};
use image::RgbImage;
use nonempty::NonEmpty;
use serde_json::Value;
use smallvec::SmallVec;
use sqlx::{postgres::PgRow, Executor, Postgres, Row};
use std::{convert::TryFrom, fmt, str::FromStr};
use uuid::Uuid;

pub mod api;

use self::api::ScryfallApi;

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct ScryfallId(Uuid);

impl ScryfallId {
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl FromStr for ScryfallId {
    type Err = <Uuid as FromStr>::Err;
    fn from_str(id: &str) -> Result<Self, Self::Err> {
        let inner = Uuid::from_str(id)?;
        Ok(ScryfallId(inner))
    }
}

impl fmt::Debug for ScryfallId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &self.as_uuid())
    }
}

impl fmt::Display for ScryfallId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.as_uuid().fmt(f)
    }
}

#[derive(Clone, Debug)]
pub struct ScryfallCard {
    json: Value,
    updated_at: DateTime<Utc>,
}

struct ScryfallCardRow {
    json: String,
    updated_at: DateTime<Utc>,
}

impl sqlx::FromRow<PgRow> for ScryfallCardRow {
    fn from_row(row: PgRow) -> Self {
        let updated_at: DateTime<Utc> = row.get("updated_at");
        let json: String = row.get("json");
        // let json: Value =
        //     serde_json::from_str(&json_text).context("Failed to parse JSON from database")?;
        ScryfallCardRow { json, updated_at }
    }
}

impl TryFrom<ScryfallCardRow> for ScryfallCard {
    type Error = Error;

    fn try_from(row: ScryfallCardRow) -> Result<ScryfallCard, Error> {
        let ScryfallCardRow { json, updated_at } = row;
        let json: Value =
            serde_json::from_str(&json).context("Failed to parse JSON from database")?;
        Ok(ScryfallCard { json, updated_at })
    }
}

impl ScryfallCard {
    pub async fn save_from_json(
        db: &mut impl Executor<Database = Postgres>,
        json: Value,
    ) -> Result<Self, Error> {
        let row = sqlx::query(
            "\
INSERT INTO scryfall_card ( json ) VALUES ( $1::jsonb )
ON CONFLICT ((json ->> 'id')) DO UPDATE SET json = $1::jsonb
RETURNING updated_at
;",
        )
        .bind(serde_json::to_string(&json)?)
        .fetch_one(db)
        .await?;
        let updated_at = row.get("updated_at");
        Ok(ScryfallCard { json, updated_at })
    }

    pub fn id(&self) -> Result<ScryfallId, Error> {
        Ok(ScryfallId::from_str(
            self.json
                .get("id")
                .ok_or_else(|| format_err!("Card JSON missing \"id\" key"))?
                .as_str()
                .ok_or_else(|| format_err!("Card JSON \"id\" field was not a string"))?,
        )?)
    }

    pub fn oracle_id(&self) -> Result<ScryfallId, Error> {
        Ok(ScryfallId::from_str(
            self.json
                .get("oracle_id")
                .ok_or_else(|| format_err!("Card JSON missing \"oracle_id\" key"))?
                .as_str()
                .ok_or_else(|| format_err!("Card JSON \"oracle_id\" field was not a string"))?,
        )?)
    }

    pub fn raw_json(&self) -> &Value {
        &self.json
    }

    pub fn combined_name(&self) -> String {
        const NAME_SEP: &'static str = " // ";
        let names = self.names();
        let first = names.first();
        let tail = names.tail();
        if tail.is_empty() {
            return first.clone();
        } else {
            let tail_joined = names.tail().join(NAME_SEP);
            [first.as_str(), tail_joined.as_str()].join(NAME_SEP)
        }
    }

    pub fn names(&self) -> NonEmpty<String> {
        let inner = move || {
            let names_str = self
                .json
                .get("name")
                .ok_or_else(|| format_err!("Card JSON missing \"name\" key"))?
                .as_str()
                .ok_or_else(|| format_err!("Card JSON \"name\" field was not a string"))?;
            let mut first_name: Option<String> = None;
            let mut rest_names: Vec<String> = vec![];
            for name in names_str.split(" // ") {
                if first_name.is_none() {
                    first_name = Some(name.to_string())
                } else {
                    rest_names.push(name.to_string());
                }
            }
            match first_name {
                None => Err(format_err!("Card {} has no names", self.id()?)),
                Some(f) => Ok(NonEmpty::from((f, rest_names))),
            }
        };
        match inner() {
            Ok(names) => names,
            Err(e) => {
                let id = self
                    .id()
                    .map(|id| format!("{}", id))
                    .unwrap_or_else(|id_err| format!("<error getting ID: {}>", id_err));
                error!("Got error when generating names for card {}: {}", id, e);
                NonEmpty::singleton(format!("<error getting name for card {}>", id))
            }
        }
    }

    pub fn legal_in(&self, format: &str) -> Result<bool, Error> {
        let legalities = self
            .json
            .get("legalities")
            .ok_or_else(|| format_err!("Card JSON missing \"legalities\" field"))?;
        let legal_or_not = legalities
            .get(format)
            .ok_or_else(|| format_err!("Unknown legality for format {:?}", format))?
            .as_str()
            .ok_or_else(|| format_err!("Card legality for format {:?} is not a string", format))?;
        match legal_or_not {
            "legal" => Ok(true),
            "not_legal" => Ok(false),
            other => Err(format_err!(
                "Found unexpected legality ruling {:?} for format {:?}",
                other,
                format
            )),
        }
    }

    // pub fn image_url(&self, format: &str) -> Result<&str, Error> {
    //     self.json
    //         .get("image_uris")
    //         .ok_or_else(|| format_err!("Card JSON missing \"image_uris\" field"))?
    //         .get(format)
    //         .ok_or_else(|| format_err!("Card JSON \"image_uris\" missing format {:?}", format))?
    //         .as_str()
    //         .ok_or_else(|| format_err!("Card JSON \"image_uris\".{:?} is not a string", format))
    // }

    pub async fn ensure_image<P: AsRef<Path>>(
        &self,
        root: P,
        api: &ScryfallApi,
    ) -> Result<RgbImage, Error> {
        let id = self.id()?;
        api.get_image_by_id(id, root, api::ImageFormat::PNG, api::ImageFace::Front)
            .await
    }

    pub fn type_line(&self) -> Result<&str, Error> {
        self.json
            .get("type_line")
            .ok_or_else(|| format_err!("Card JSON missing \"type_line\" field"))?
            .as_str()
            .ok_or_else(|| format_err!("Card JSON \"type_line\" field was not a string"))
    }

    pub fn types(&self) -> Result<SmallVec<[&str; 4]>, Error> {
        let mut types = SmallVec::new();
        let type_line = self.type_line()?;
        for card_type in type_line.split_whitespace() {
            if card_type == "—" {
                break;
            }
            types.push(card_type);
        }
        Ok(types)
    }

    pub fn basic_land_type(&self) -> Option<BasicLandType> {
        match self.type_line().ok()? {
            "Basic Land — Forest" => Some(BasicLandType::Forest),
            "Basic Land — Island" => Some(BasicLandType::Island),
            "Basic Land — Mountain" => Some(BasicLandType::Mountain),
            "Basic Land — Plains" => Some(BasicLandType::Plains),
            "Basic Land — Swamp" => Some(BasicLandType::Swamp),
            _ => None,
        }
    }

    // pub fn subtypes(&self) -> Result<SmallVec<[&str; 4]>, Error> {
    //     let mut subtypes = SmallVec::new();
    //     let type_line = self.type_line()?;
    //     let mut seen_dash = false;
    //     for card_type in type_line.split_whitespace() {
    //         if card_type == "—" {
    //             seen_dash = true;
    //             continue;
    //         }
    //         if seen_dash {
    //             subtypes.push(card_type);
    //         }
    //     }
    //     Ok(subtypes)
    // }

    pub fn oracle_text(&self) -> Result<&str, Error> {
        self.json
            .get("oracle_text")
            .ok_or_else(|| format_err!("Card JSON missing \"oracle_text\" field"))?
            .as_str()
            .ok_or_else(|| format_err!("Card JSON \"oracle_text\" field is not a string"))
    }

    pub fn can_be_a_commander(&self) -> Result<bool, Error> {
        let types = self.types()?;
        let legendary_creature = types.contains(&"Legendary") && types.contains(&"Creature");
        let oracle_text = self.oracle_text().unwrap_or("");
        let explicitly_allowed = self
            .names()
            .iter()
            .any(|n| oracle_text.contains(&format!("{} can be your commander", n)));
        Ok(legendary_creature || explicitly_allowed)
    }
}

#[inline]
async fn ensure<F, T, G, U, H, V>(
    cache_duration: Duration,
    get_card: F,
    api_lookup: G,
    upsert: H,
) -> Result<ScryfallCard, Error>
where
    F: FnOnce() -> T,
    T: Future<Output = Result<Option<ScryfallCard>, Error>>,
    G: FnOnce() -> U,
    U: Future<Output = Result<Value, Error>>,
    H: FnOnce(ScryfallId, Value, DateTime<Utc>) -> V,
    V: Future<Output = Result<(), Error>>,
{
    let now = Utc::now();
    let orig_card_opt = get_card()
        .await
        .context(format!("Looking up card in ensure before network"))?;
    if let Some(c) = orig_card_opt {
        let threshold = now - cache_duration;
        if c.updated_at > threshold {
            return Ok(c);
        }
    }

    let card_json = api_lookup().await?;
    let raw_id = card_json
        .get("id")
        .ok_or_else(|| format_err!("JSON response from Scryfall is missing \"id\" field"))?
        .as_str()
        .ok_or_else(|| format_err!("JSON reponse from Scryfall's \"id\" field was not a string"))?;
    let id = ScryfallId::from_str(raw_id)?;
    let card = ScryfallCard {
        json: card_json.clone(),
        updated_at: now,
    };
    upsert(id, card_json, now).await?;
    Ok(card)
}

async fn card_by_id<DB>(db: &mut DB, id: ScryfallId) -> Result<Option<ScryfallCard>, Error>
where
    DB: Executor<Database = Postgres>,
{
    debug!("Checking database for card with Scryfall ID {}", id);
    let row_opt: Option<ScryfallCardRow> = sqlx::query_as(
        "\
SELECT json::text, updated_at FROM scryfall_card
WHERE
    (json ->> 'id')::uuid = $1
;",
    )
    .bind(id.as_uuid())
    .fetch_optional(db)
    .await?;
    match row_opt {
        None => Ok(None),
        Some(row) => match ScryfallCard::try_from(row) {
            Ok(card) => Ok(Some(card)),
            Err(e) => Err(e),
        },
    }
}

#[derive(Copy, Clone, Debug)]
pub enum BasicLandType {
    Forest,
    Island,
    Mountain,
    Plains,
    Swamp,
}

impl BasicLandType {
    fn oracle_id(self) -> Uuid {
        match self {
            Self::Forest => Uuid::from_u128(0xb34bb2dc_c1af_4d77_b0b3_a0fb342a5fc6),
            Self::Island => Uuid::from_u128(0xb2c6aa39_2d2a_459c_a555_fb48ba993373),
            Self::Mountain => Uuid::from_u128(0xa3fb7228_e76b_4e96_a40e_20b5fed75685),
            Self::Plains => Uuid::from_u128(0xbc71ebf6_2056_41f7_be35_b2e5c34afa99),
            Self::Swamp => Uuid::from_u128(0x56719f6a_1a6c_4c0a_8d21_18f7d7350b68),
        }
    }
}

pub async fn full_faced_lands<DB>(
    db: &mut DB,
    basic_land_type: BasicLandType,
) -> Result<Vec<ScryfallCard>, Error>
where
    DB: Executor<Database = Postgres>,
{
    let oracle_id = basic_land_type.oracle_id();
    debug!(
        "Checking database for cards with Scryfall oracle ID {}",
        oracle_id
    );
    let raw_cards: Vec<ScryfallCardRow> = sqlx::query_as(
        "\
SELECT json::text, updated_at FROM scryfall_card
WHERE
    (json ->> 'oracle_id')::uuid = $1
AND (json ->> 'full_art')::boolean = true
AND (json ->> 'set_type') != 'funny'
;",
    )
    .bind(oracle_id)
    .fetch_all(db)
    .await?;
    let mut cards = Vec::with_capacity(raw_cards.len());
    for raw_card in raw_cards {
        let card = ScryfallCard::try_from(raw_card)?;
        cards.push(card);
    }
    Ok(cards)
}

async fn card_by_name<DB>(db: &mut DB, name: &str) -> Result<Option<ScryfallCard>, Error>
where
    DB: Executor<Database = Postgres>,
{
    // let name_array: &[&str] = &[name];
    debug!("Checking database for card with name \"{}\"", name);
    let row_opt: Option<ScryfallCardRow> = sqlx::query_as(
        //     $1 = ANY (string_to_array((json ->> 'name'), ' // '))
        "\
SELECT json::text, updated_at FROM scryfall_card
WHERE
    string_to_array((json ->> 'name'), ' // ') @> (array_append('{}'::text[], $1))
ORDER BY (json->>'released_at')::date DESC
LIMIT 1
;
",
    )
    .bind(name)
    .fetch_optional(db)
    .await?;
    match row_opt {
        None => Ok(None),
        Some(row) => match ScryfallCard::try_from(row) {
            Ok(card) => Ok(Some(card)),
            Err(e) => Err(e),
        },
    }
}

// pub async fn lookup_card(
//     db: &mut impl Executor<Database = Postgres>,
//     id: ScryfallId,
// ) -> Result<Option<ScryfallCard>, Error> {
//     card_by_id(db, id).await
// }

const CARD_CACHE_DAYS: i64 = 14;

pub async fn ensure_card(
    api: &ScryfallApi,
    db: &mut impl Executor<Database = Postgres>,
    id: ScryfallId,
) -> Result<ScryfallCard, Error> {
    use std::cell::Cell;
    use std::rc::Rc;

    let db_lookup = Rc::new(Cell::new(Some(db)));
    let db_upsert = Rc::clone(&db_lookup);
    ensure(
        Duration::days(CARD_CACHE_DAYS),
        || {
            async {
                let db: &mut _ = db_lookup.replace(None).unwrap();
                let row_opt = card_by_id(db, id).await?;
                db_lookup.replace(Some(db));
                Ok(row_opt)
            }
        },
        move || {
            async move {
                let c = api.lookup_by_id(id).await?;
                Ok(c)
            }
        },
        |id, card_json, updated_at| {
            async move {
                const UPSERT_QUERY: &'static str = "\
INSERT INTO scryfall_card ( json, updated_at )
VALUES ( $1::jsonb, $2 )
    ON CONFLICT (( json ->> 'id' )) DO UPDATE
    SET json = $1::jsonb, updated_at = $2;
";
                info!("Upserting card Scryfall ID {} to the DB", id);
                let db: &mut _ = db_upsert.replace(None).unwrap();
                sqlx::query(UPSERT_QUERY)
                    .bind(serde_json::to_string(&card_json)?)
                    .bind(updated_at)
                    .execute(db)
                    .await?;
                db_upsert.replace(Some(db));
                Ok(())
            }
        },
    )
    .await
}

// pub async fn lookup_card_by_name(
//     db: &mut impl Executor<Database = Postgres>,
//     name: &str,
// ) -> Result<Option<ScryfallCard>, Error> {
//     card_by_name(db, name).await
// }

pub async fn ensure_card_by_name(
    api: &ScryfallApi,
    db: &mut impl Executor<Database = Postgres>,
    name: &str,
) -> Result<ScryfallCard, Error> {
    use std::cell::Cell;
    use std::rc::Rc;

    let db_lookup = Rc::new(Cell::new(Some(db)));
    let db_upsert = Rc::clone(&db_lookup);
    ensure(
        Duration::days(CARD_CACHE_DAYS),
        || {
            async {
                let db: &mut _ = db_lookup.replace(None).unwrap();
                let row_opt = card_by_name(db, name).await?;
                db_lookup.replace(Some(db));
                Ok(row_opt)
            }
        },
        move || {
            async move {
                let c = api.lookup_by_name(name).await?;
                Ok(c)
            }
        },
        |id, card_json, updated_at| {
            async move {
                const UPSERT_QUERY: &'static str = "\
INSERT INTO scryfall_card ( json, updated_at )
VALUES ( $1::jsonb, $2 )
    ON CONFLICT (( json ->> 'id' )) DO UPDATE
    SET json = $1::jsonb, updated_at = $2;
";
                info!("Upserting card Scryfall ID {} to the DB", id);
                let db: &mut _ = db_upsert.replace(None).unwrap();
                sqlx::query(UPSERT_QUERY)
                    .bind(serde_json::to_string(&card_json)?)
                    .bind(updated_at)
                    .execute(db)
                    .await?;
                db_upsert.replace(Some(db));
                Ok(())
            }
        },
    )
    .await
}

pub async fn load_bulk<P: AsRef<Path>>(
    api: &ScryfallApi,
    db: &mut impl Executor<Database = Postgres>,
    root: P,
    force: bool,
) -> Result<(), Error> {
    use async_std::{fs, prelude::*};
    let bulk_dir = root.as_ref().join("files").join("bulk");
    fs::create_dir_all(&bulk_dir).await?;
    let cards_filename = bulk_dir.join("default_cards.json");
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 23);
    let download = if force {
        warn!("Forcibly redownloading from Scryfall");
        true
    } else if !cards_filename.is_file().await {
        warn!(
            "Cache file {} is missing, downloading from Scryfall",
            cards_filename.to_string_lossy()
        );
        true
    } else if cards_filename.metadata().await?.modified()? < cutoff {
        warn!(
            "Cache file {} was modified before the cutoff timestamp, downloading from Scryfall",
            cards_filename.to_string_lossy(),
        );
        true
    } else {
        false
    };
    let value = if download {
        debug!(
            "Cached file {} is out of date or non-existent, downloading fresh",
            cards_filename.to_string_lossy()
        );
        let value = api.get_bulk_data("default_cards").await?;
        let mut f = fs::File::create(&cards_filename).await?;
        let bytes = serde_json::to_vec(&value)?;
        f.write_all(bytes.as_slice()).await?;
        value
    } else {
        debug!("Using cached file {}", cards_filename.to_string_lossy());
        let mut f = fs::File::open(&cards_filename).await?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).await?;
        serde_json::from_slice(bytes.as_slice())?
    };

    let cards = value
        .as_array()
        .ok_or_else(|| failure::format_err!("default_cards.json is not an array"))?;
    info!(
        "Saving {} cards from Scryfall into database...",
        cards.len()
    );
    use indicatif::ProgressIterator;
    for card in cards.into_iter().progress() {
        ScryfallCard::save_from_json(db, card.clone()).await?;
    }
    Ok(())
}
