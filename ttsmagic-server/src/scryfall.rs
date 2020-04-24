use anyhow::{anyhow, Context, Result};
use async_std::{path::Path, prelude::*};
use chrono::prelude::*;
use image::RgbImage;
use nonempty::NonEmpty;
use pbr::PbIter;
use serde_json::Value;
// use smallvec::SmallVec;
use sqlx::{postgres::PgRow, Executor, Postgres, Row};
use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom,
    fmt,
    str::FromStr,
};
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

impl From<Uuid> for ScryfallId {
    fn from(uuid: Uuid) -> ScryfallId {
        ScryfallId(uuid)
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

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct ScryfallOracleId(Uuid);

impl ScryfallOracleId {
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl FromStr for ScryfallOracleId {
    type Err = <Uuid as FromStr>::Err;
    fn from_str(id: &str) -> Result<Self, Self::Err> {
        let inner = Uuid::from_str(id)?;
        Ok(ScryfallOracleId(inner))
    }
}

impl fmt::Debug for ScryfallOracleId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", &self.as_uuid())
    }
}

impl fmt::Display for ScryfallOracleId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.as_uuid().fmt(f)
    }
}

#[derive(Clone, Debug)]
pub struct ScryfallCard {
    json: Value,
    updated_at: DateTime<Utc>,
}

pub struct ScryfallCardRow {
    pub json: String,
    pub updated_at: DateTime<Utc>,
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
    type Error = anyhow::Error;

    fn try_from(row: ScryfallCardRow) -> Result<ScryfallCard> {
        let ScryfallCardRow { json, updated_at } = row;
        let json: Value = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to parse Scryfall card JSON: {}", e);
                debug!("(bad JSON: {:?})", json);
                Err(e).context("Failed to parse JSON from database")?
            }
        };
        Ok(ScryfallCard { json, updated_at })
    }
}

impl ScryfallCard {
    pub async fn save_from_json(
        db: &mut impl Executor<Database = Postgres>,
        json: Value,
    ) -> Result<Self> {
        let row = sqlx::query(
            "\
INSERT INTO scryfall_card ( json ) VALUES ( $1::jsonb )
ON CONFLICT (((json ->> 'id'::text)::uuid)) DO UPDATE SET json = $1::jsonb
RETURNING updated_at
;",
        )
        .bind(serde_json::to_string(&json)?)
        .fetch_one(db)
        .await?;
        let updated_at = row.get("updated_at");
        Ok(ScryfallCard { json, updated_at })
    }

    pub fn id(&self) -> Result<ScryfallId> {
        Ok(ScryfallId::from_str(
            self.json
                .get("id")
                .ok_or_else(|| anyhow!("Card JSON missing \"id\" key"))?
                .as_str()
                .ok_or_else(|| anyhow!("Card JSON \"id\" field was not a string"))?,
        )?)
    }

    pub fn oracle_id(&self) -> Result<ScryfallOracleId> {
        Ok(ScryfallOracleId::from_str(
            self.json
                .get("oracle_id")
                .ok_or_else(|| anyhow!("Card JSON missing \"oracle_id\" key"))?
                .as_str()
                .ok_or_else(|| anyhow!("Card JSON \"oracle_id\" field was not a string"))?,
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
                .ok_or_else(|| anyhow!("Card JSON missing \"name\" key"))?
                .as_str()
                .ok_or_else(|| anyhow!("Card JSON \"name\" field was not a string"))?;
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
                None => Err(anyhow!("Card {} has no names", self.id()?)),
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

    // pub fn legal_in(&self, format: &str) -> Result<bool> {
    //     let legalities = self
    //         .json
    //         .get("legalities")
    //         .ok_or_else(|| anyhow!("Card JSON missing \"legalities\" field"))?;
    //     let legal_or_not = legalities
    //         .get(format)
    //         .ok_or_else(|| anyhow!("Unknown legality for format {:?}", format))?
    //         .as_str()
    //         .ok_or_else(|| anyhow!("Card legality for format {:?} is not a string", format))?;
    //     match legal_or_not {
    //         "legal" => Ok(true),
    //         "not_legal" => Ok(false),
    //         other => Err(anyhow!(
    //             "Found unexpected legality ruling {:?} for format {:?}",
    //             other,
    //             format
    //         )),
    //     }
    // }

    // pub fn image_url(&self, format: &str) -> Result<&str> {
    //     self.json
    //         .get("image_uris")
    //         .ok_or_else(|| anyhow!("Card JSON missing \"image_uris\" field"))?
    //         .get(format)
    //         .ok_or_else(|| anyhow!("Card JSON \"image_uris\" missing format {:?}", format))?
    //         .as_str()
    //         .ok_or_else(|| anyhow!("Card JSON \"image_uris\".{:?} is not a string", format))
    // }

    pub async fn ensure_image<P: AsRef<Path>>(
        &self,
        root: P,
        api: &ScryfallApi,
    ) -> Result<RgbImage> {
        let id = self.id()?;
        api.get_image_by_id(id, root, api::ImageFormat::PNG, api::ImageFace::Front)
            .await
    }

    pub fn description(&self) -> Result<String> {
        match (self.cost(), self.type_line(), self.oracle_text()) {
            (Ok(cost), Ok(tl), Ok(text)) => Ok(format!("{}\n\n{}\n\n{}", cost, tl, text)),
            (Ok(cost), Ok(tl), Err(_)) => Ok(format!("{}\n\n{}", cost, tl)),
            (Ok(cost), Err(_), Ok(text)) => Ok(format!("{}\n\n{}", cost, text)),
            (Ok(cost), Err(_), Err(_)) => Ok(format!("{}", cost)),
            (Err(_), Ok(tl), Ok(text)) => Ok(format!("{}\n\n{}", tl, text)),
            (Err(_), Ok(tl), Err(_)) => Ok(format!("{}", tl)),
            (Err(_), Err(_), Ok(text)) => Ok(format!("{}", text)),
            (Err(e), Err(_), Err(_)) => Err(e),
        }
    }

    pub fn cost(&self) -> Result<&str> {
        self.json
            .get("mana_cost")
            .ok_or_else(|| anyhow!("Card JSON missing \"mana_cost\" field"))?
            .as_str()
            .ok_or_else(|| anyhow!("Card JSON \"mana_cost\" field was not a string"))
    }

    // pub fn color_identity(&self) -> Result<HashSet<&str>> {
    //     let color_values = self
    //         .json
    //         .get("color_identity")
    //         .ok_or_else(|| anyhow!("Card JSON missing \"color_identity\" field"))?
    //         .as_array()
    //         .ok_or_else(|| anyhow!("Card JSON \"color_identity\" field was not an array"))?;
    //     let mut colors = HashSet::with_capacity(color_values.len());
    //     for cv in color_values {
    //         let color = cv
    //             .as_str()
    //             .ok_or_else(|| anyhow!("Color identity value {:?} was not a string", cv))?;
    //         colors.insert(color);
    //     }
    //     Ok(colors)
    // }

    pub fn type_line(&self) -> Result<&str> {
        self.json
            .get("type_line")
            .ok_or_else(|| anyhow!("Card JSON missing \"type_line\" field"))?
            .as_str()
            .ok_or_else(|| anyhow!("Card JSON \"type_line\" field was not a string"))
    }

    // pub fn types(&self) -> Result<SmallVec<[&str; 4]>> {
    //     let mut types = SmallVec::new();
    //     let type_line = self.type_line()?;
    //     for card_type in type_line.split_whitespace() {
    //         if card_type == "—" {
    //             break;
    //         }
    //         types.push(card_type);
    //     }
    //     Ok(types)
    // }

    // pub fn basic_land_type(&self) -> Option<BasicLandType> {
    //     match self.type_line().ok()? {
    //         "Basic Land — Forest" => Some(BasicLandType::Forest),
    //         "Basic Land — Island" => Some(BasicLandType::Island),
    //         "Basic Land — Mountain" => Some(BasicLandType::Mountain),
    //         "Basic Land — Plains" => Some(BasicLandType::Plains),
    //         "Basic Land — Swamp" => Some(BasicLandType::Swamp),
    //         _ => None,
    //     }
    // }

    // pub fn subtypes(&self) -> Result<SmallVec<[&str; 4]>> {
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

    pub fn oracle_text(&self) -> Result<&str> {
        self.json
            .get("oracle_text")
            .ok_or_else(|| anyhow!("Card JSON missing \"oracle_text\" field"))?
            .as_str()
            .ok_or_else(|| anyhow!("Card JSON \"oracle_text\" field is not a string"))
    }

    // pub fn can_be_a_commander(&self) -> Result<bool> {
    //     let types = self.types()?;
    //     let legendary_creature = types.contains(&"Legendary") && types.contains(&"Creature");
    //     let oracle_text = self.oracle_text().unwrap_or("");
    //     let explicitly_allowed = self
    //         .names()
    //         .iter()
    //         .any(|n| oracle_text.contains(&format!("{} can be your commander", n)));
    //     Ok(legendary_creature || explicitly_allowed)
    // }
}

// #[inline]
// async fn ensure<F, T, G, U, H, V>(
//     cache_duration: Duration,
//     get_card: F,
//     api_lookup: G,
//     upsert: H,
// ) -> Result<ScryfallCard>
// where
//     F: FnOnce() -> T,
//     T: Future<Output = Result<Option<ScryfallCard>>>,
//     G: FnOnce() -> U,
//     U: Future<Output = Result<Vec<Value>>>,
//     H: FnOnce(ScryfallId, Value, DateTime<Utc>) -> V,
//     V: Future<Output = Result<()>>,
// {
//     let now = Utc::now();
//     let orig_card_opt = get_card()
//         .await
//         .context(format!("Looking up card in ensure before network"))?;
//     if let Some(c) = orig_card_opt {
//         let threshold = now - cache_duration;
//         if c.updated_at > threshold {
//             return Ok(c);
//         }
//     }

//     let card_json = api_lookup().await?;
//     let raw_id = card_json
//         .get("id")
//         .ok_or_else(|| anyhow!("JSON response from Scryfall is missing \"id\" field"))?
//         .as_str()
//         .ok_or_else(|| anyhow!("JSON reponse from Scryfall's \"id\" field was not a string"))?;
//     let id = ScryfallId::from_str(raw_id)?;
//     let card = ScryfallCard {
//         json: card_json.clone(),
//         updated_at: now,
//     };
//     upsert(id, card_json, now).await?;
//     Ok(card)
// }

pub async fn card_by_id(
    db: &mut impl Executor<Database = Postgres>,
    id: ScryfallId,
) -> Result<ScryfallCard> {
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
        None => Err(anyhow!(
            "Failed to find a matching card with Scryfall ID {}",
            id
        )),
        Some(row) => match ScryfallCard::try_from(row) {
            Ok(card) => Ok(card),
            Err(e) => Err(e),
        },
    }
}

// #[derive(Copy, Clone, Debug)]
// pub enum BasicLandType {
//     Forest,
//     Island,
//     Mountain,
//     Plains,
//     Swamp,
// }

// impl BasicLandType {
//     fn oracle_id(self) -> Uuid {
//         match self {
//             Self::Forest => Uuid::from_u128(0xb34bb2dc_c1af_4d77_b0b3_a0fb342a5fc6),
//             Self::Island => Uuid::from_u128(0xb2c6aa39_2d2a_459c_a555_fb48ba993373),
//             Self::Mountain => Uuid::from_u128(0xa3fb7228_e76b_4e96_a40e_20b5fed75685),
//             Self::Plains => Uuid::from_u128(0xbc71ebf6_2056_41f7_be35_b2e5c34afa99),
//             Self::Swamp => Uuid::from_u128(0x56719f6a_1a6c_4c0a_8d21_18f7d7350b68),
//         }
//     }
// }

// pub async fn full_faced_lands<DB>(
//     db: &mut DB,
//     basic_land_type: BasicLandType,
// ) -> Result<Vec<ScryfallCard>>
// where
//     DB: Executor<Database = Postgres>,
// {
//     let oracle_id = basic_land_type.oracle_id();
//     debug!(
//         "Checking database for cards with Scryfall oracle ID {}",
//         oracle_id
//     );
//     let raw_cards: Vec<ScryfallCardRow> = sqlx::query_as(
//         "\
// SELECT json::text, updated_at FROM scryfall_card
// WHERE
//     (json ->> 'oracle_id')::uuid = $1
// AND (json ->> 'full_art')::boolean = true
// AND (json ->> 'set_type') != 'funny'
// AND (json ->> 'lang') = 'en'
// ;",
//     )
//     .bind(oracle_id)
//     .fetch_all(db)
//     .await?;
//     let mut cards = Vec::with_capacity(raw_cards.len());
//     for raw_card in raw_cards {
//         let card = ScryfallCard::try_from(raw_card)?;
//         cards.push(card);
//     }
//     Ok(cards)
// }

pub async fn oracle_id_by_name(
    db: &mut impl Executor<Database = Postgres>,
    name: &str,
) -> Result<ScryfallOracleId> {
    debug!("Checking database for card with name \"{}\"", name);
    let row = sqlx::query(
        "\
SELECT (json ->> 'oracle_id')::uuid AS oracle_id FROM scryfall_card
WHERE
    string_to_array(lower(json ->> 'name'), ' // ') @> string_to_array(lower($1), ' // ')
AND (json ->> 'oracle_id')::uuid IS NOT NULL
LIMIT 1
;",
    )
    .bind(name)
    .fetch_one(db)
    .await
    .with_context(|| {
        format!(
            "Checking database for card with name {:?} for oracle ID",
            name
        )
    })?;
    let oracle_id = row.get("oracle_id");
    Ok(ScryfallOracleId(oracle_id))
}

// pub async fn lookup_card(
//     db: &mut impl Executor<Database = Postgres>,
//     id: ScryfallId,
// ) -> Result<Option<ScryfallCard>> {
//     card_by_id(db, id).await
// }

// const CARD_CACHE_DAYS: i64 = 14;

// pub async fn ensure_card(
//     api: &ScryfallApi,
//     db: &mut impl Executor<Database = Postgres>,
//     id: ScryfallId,
// ) -> Result<ScryfallCard> {
//     use std::cell::Cell;
//     use std::rc::Rc;

//     let db_lookup = Rc::new(Cell::new(Some(db)));
//     let db_upsert = Rc::clone(&db_lookup);
//     ensure(
//         Duration::days(CARD_CACHE_DAYS),
//         || {
//             async {
//                 let db: &mut _ = db_lookup.replace(None).unwrap();
//                 let row_opt = card_by_id(db, id).await?;
//                 db_lookup.replace(Some(db));
//                 Ok(row_opt)
//             }
//         },
//         move || {
//             async move {
//                 let c = api.lookup_by_id(id).await?;
//                 Ok(vec![c])
//             }
//         },
//         |id, card_json, updated_at| {
//             async move {
//                 const UPSERT_QUERY: &'static str = "\
// INSERT INTO scryfall_card ( json, updated_at )
// VALUES ( $1::jsonb, $2 )
//     ON CONFLICT (( json ->> 'id' )) DO UPDATE
//     SET json = $1::jsonb, updated_at = $2;
// ";
//                 info!("Upserting card Scryfall ID {} to the DB", id);
//                 let db: &mut _ = db_upsert.replace(None).unwrap();
//                 sqlx::query(UPSERT_QUERY)
//                     .bind(serde_json::to_string(&card_json)?)
//                     .bind(updated_at)
//                     .execute(db)
//                     .await?;
//                 db_upsert.replace(Some(db));
//                 Ok(())
//             }
//         },
//     )
//     .await
// }

// pub async fn lookup_card_by_name(
//     db: &mut impl Executor<Database = Postgres>,
//     name: &str,
// ) -> Result<Option<ScryfallCard>> {
//     card_by_name(db, name).await
// }

// pub async fn ensure_oracle_id_by_name(
//     api: &ScryfallApi,
//     db: &mut impl Executor<Database = Postgres>,
//     name: &str,
// ) -> Result<ScryfallOracleId> {
//     use std::cell::Cell;
//     use std::rc::Rc;

//     let db_lookup = Rc::new(Cell::new(Some(db)));
//     let db_upsert = Rc::clone(&db_lookup);
//     let card = ensure(
//         Duration::days(CARD_CACHE_DAYS),
//         || {
//             async {
//                 let db: &mut _ = db_lookup.replace(None).unwrap();
//                 let row_opt = oracle_id_by_name(db, name).await?;
//                 db_lookup.replace(Some(db));
//                 Ok(row_opt)
//             }
//         },
//         move || {
//             async move {
//                 let cs = api.lookup_by_name(name).await?;
//                 Ok(cs)
//             }
//         },
//         |id, card_json, updated_at| {
//             async move {
//                 const UPSERT_QUERY: &'static str = "\
// INSERT INTO scryfall_card ( json, updated_at )
// VALUES ( $1::jsonb, $2 )
//     ON CONFLICT (( json ->> 'id' )) DO UPDATE
//     SET json = $1::jsonb, updated_at = $2;
// ";
//                 info!("Upserting card Scryfall ID {} to the DB", id);
//                 let db: &mut _ = db_upsert.replace(None).unwrap();
//                 sqlx::query(UPSERT_QUERY)
//                     .bind(serde_json::to_string(&card_json)?)
//                     .bind(updated_at)
//                     .execute(db)
//                     .await?;
//                 db_upsert.replace(Some(db));
//                 Ok(())
//             }
//         },
//     )
//     .await?;
//     card.oracle_id()
// }

pub async fn load_bulk<P: AsRef<Path>>(
    api: &ScryfallApi,
    db: &mut impl Executor<Database = Postgres>,
    root: P,
    force: bool,
) -> Result<()> {
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
        .ok_or_else(|| anyhow!("default_cards.json is not an array"))?;
    info!(
        "Saving {} cards from Scryfall into database...",
        cards.len()
    );
    for card in PbIter::new(cards.into_iter()) {
        ScryfallCard::save_from_json(db, card.clone()).await?;
    }
    Ok(())
}

// Expand a single oracle ID into multiple printed cards. We prefer full faced
// cards when available.
pub async fn expand_oracle_id(
    db: &mut impl Executor<Database = Postgres>,
    oracle_id: ScryfallOracleId,
    oracle_count: u8,
) -> Result<Vec<(ScryfallId, ScryfallCard, u8)>> {
    let mut rows_stream = sqlx::query_as(
        "\
SELECT json::text, updated_at FROM scryfall_card
WHERE
    (json ->> 'oracle_id')::uuid = $1
AND (json ->> 'lang') = 'en'
ORDER BY
    (json ->> 'released_at')::date DESC,
    json ->> 'collector_number' ASC
;",
    )
    .bind(oracle_id.as_uuid())
    .fetch(db);
    let mut rows: Vec<ScryfallCardRow> = vec![];
    while let Some(row_result) = rows_stream.next().await {
        let row = row_result?;
        rows.push(row);
    }
    // let rows: Vec<ScryfallCardRow> = futures::future::FutureExt::map(
    //     rows_stream.collect(),
    //     |rows_result: Vec<Result<ScryfallCardRow, sqlx::Error>>| {
    //         rows_result
    //             .into_iter()
    //             .collect::<Result<Vec<ScryfallCardRow>, sqlx::Error>>()
    //     },
    // )
    // .await?;
    // let rows = rows_result.into_iter().collect::<Result<Vec<_>, _>>()?;
    let mut by_key: HashMap<(bool, bool), Vec<ScryfallCard>> = HashMap::new();
    for row in rows {
        match ScryfallCard::try_from(row) {
            Ok(c) => {
                let full_face = c
                    .raw_json()
                    .get("full_art")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let set_type = c
                    .raw_json()
                    .get("set_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let key = (full_face, set_type == "funny");
                by_key.entry(key).or_insert(vec![]).push(c);
            }
            Err(e) => {
                warn!(
                    "Failed to parse a card from the database with oracle ID {}: {}",
                    oracle_id, e
                );
                continue;
            }
        }
    }

    let options: &[ScryfallCard] = by_key
        .get(&(true, false))
        .map(Vec::as_slice)
        .or_else(|| by_key.get(&(false, false)).map(Vec::as_slice))
        .or_else(|| by_key.get(&(true, true)).map(Vec::as_slice))
        .or_else(|| by_key.get(&(false, true)).map(Vec::as_slice))
        .unwrap_or(&[]);
    if options.is_empty() {
        return Err(anyhow!(
            "Failed to find any cards matching oracle ID {}",
            oracle_id
        ));
    }

    let mut by_id: HashMap<ScryfallId, (ScryfallCard, u8)> = HashMap::new();
    for i in 0..(oracle_count as usize) {
        let card = &options[i % options.len()];
        let card_id = card.id()?;
        let entry = by_id.entry(card_id).or_insert((card.clone(), 0));
        entry.1 += 1;
    }
    let mut cards = vec![];
    let mut card_count = 0;
    for (card_id, (card, count)) in by_id {
        cards.push((card_id, card, count));
        card_count += count;
    }
    assert_eq!(card_count, oracle_count);

    Ok(cards)
}

pub async fn check_legality_by_oracle_id(
    db: &mut impl Executor<Database = Postgres>,
    oracle_id: ScryfallOracleId,
    format: &str,
) -> Result<bool> {
    let opt_row = sqlx::query(
        "\
SELECT 1 FROM scryfall_card
WHERE
    (json ->> 'oracle_id')::uuid = $1
AND (json -> 'legalities' ->> $2)::text = 'legal'
LIMIT 1
;",
    )
    .bind(oracle_id.as_uuid())
    .bind(format)
    .fetch_optional(db)
    .await?;
    Ok(opt_row.is_some())
}

struct TextArray<'a> {
    n: u16,
    db_type: &'static str,
    strings: &'a [&'a str],
}

impl<'a> TextArray<'a> {
    fn new(n: u16, db_type: &'static str, strings: &'a [&'a str]) -> Self {
        Self {
            n,
            db_type,
            strings,
        }
    }

    fn add_bindings<Q: BindableQuery>(self, mut query: Q) -> Q {
        for s in self.strings {
            query = query.bind_value::<String>(s.to_string());
        }
        query
    }
}

trait BindableQuery<DB = Postgres>: Sized {
    fn bind_value<T>(self, value: T) -> Self
    where
        DB: sqlx::types::HasSqlType<T>,
        T: sqlx::encode::Encode<DB>;
}

impl<'q, DB: sqlx::Database> BindableQuery<DB> for sqlx::Query<'q, DB> {
    fn bind_value<T>(self, value: T) -> Self
    where
        DB: sqlx::types::HasSqlType<T>,
        T: sqlx::encode::Encode<DB>,
    {
        self.bind(value)
    }
}

impl<'q, DB: sqlx::Database, R> BindableQuery<DB> for sqlx::QueryAs<'q, DB, R> {
    fn bind_value<T>(self, value: T) -> Self
    where
        DB: sqlx::types::HasSqlType<T>,
        T: sqlx::encode::Encode<DB>,
    {
        self.bind(value)
    }
}

impl<'a> fmt::Display for TextArray<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ARRAY[")?;
        for (i, _) in self.strings.into_iter().enumerate() {
            if i == 0 {
                write!(f, "${}", (self.n as usize) + i)?;
            } else {
                write!(f, ", ${}", (self.n as usize) + i)?;
            }
        }
        write!(f, "]::{}[]", self.db_type)
    }
}

pub async fn deck_color_identity(
    db: &mut impl Executor<Database = Postgres>,
    oracle_ids: &[ScryfallOracleId],
) -> Result<HashSet<String>> {
    let oracle_id_strings = oracle_ids
        .into_iter()
        .map(|oid| format!("{}", oid))
        .collect::<Vec<String>>();
    let borrowed_ids = oracle_id_strings
        .iter()
        .map(String::as_str)
        .collect::<Vec<&str>>();
    let params = TextArray::new(1, "uuid", borrowed_ids.as_slice());
    let sql = format!(
        "\
SELECT DISTINCT jsonb_array_elements_text(json -> 'color_identity') as color FROM scryfall_card
WHERE ((json ->> 'oracle_id')::uuid) = ANY ({})
;",
        params,
    );
    let query = sqlx::query(&sql);
    let mut rows_stream = params.add_bindings(query).fetch(db);
    let mut colors = HashSet::new();
    while let Some(row_result) = rows_stream.next().await {
        let row = row_result?;
        let color: String = row.get("color");
        assert_eq!(color.len(), 1, "bad color string from DB: {:?}", color);
        // let color = row.get("color");
        // if color.starts_with("\u{1}\"") {
        // }
        colors.insert(color);
    }
    Ok(colors)
}

pub async fn can_be_a_commander(
    db: &mut impl Executor<Database = Postgres>,
    oracle_id: ScryfallOracleId,
    deck_color_identity: &[&str],
) -> Result<bool> {
    let color_identity_parms = TextArray::new(2, "text", deck_color_identity);
    let sql = format!(
        "\
SELECT 1 FROM scryfall_card
WHERE
    (json ->> 'oracle_id')::uuid = $1
AND (json -> 'color_identity') @> to_jsonb({color_identity})
AND to_jsonb({color_identity}) @> (json -> 'color_identity')
AND (
        (regexp_split_to_array(split_part((json ->> 'type_line'), ' —', 1), '\\s+') @> ARRAY['Legendary','Creature'])
     OR (position(((json ->> 'name') || ' can be your commander') IN (json ->> 'oracle_text')) > 0)
)
LIMIT 1
;",
        color_identity=color_identity_parms,
    );
    let query = sqlx::query(&sql).bind(oracle_id.as_uuid());
    let query = color_identity_parms.add_bindings(query);
    let opt_row = query.fetch_optional(db).await?;
    Ok(opt_row.is_some())
}
