use anyhow::{anyhow, Context as _, Result};
use async_std::{path::Path, prelude::*};
use chrono::prelude::*;
use image::RgbImage;
use nonempty::NonEmpty;
use serde_json::Value;
use sqlx::{
    postgres::{PgArguments, PgRow},
    Executor, PgConnection, Postgres, Row,
};
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

impl sqlx::FromRow<'_, PgRow> for ScryfallCardRow {
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        let updated_at: DateTime<Utc> = row.get("updated_at");
        let json: String = row.get("json");
        let row = ScryfallCardRow { json, updated_at };
        Ok(row)
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
        db: impl sqlx::Executor<'_, Database = Postgres>,
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
                let id: String = match self.id() {
                    Ok(id) => id.to_string(),
                    Err(e) => format!("<no card ID due to error: {:#}>", e),
                };
                let e = e.context(format!("Failed to get names for card with ID {}", id));
                sentry::integrations::anyhow::capture_anyhow(&e);
                error!("Got error when generating names for card {}: {}", id, e);
                NonEmpty::singleton(format!("<error getting name for card {}>", id))
            }
        }
    }

    pub async fn ensure_image(&self, api: &ScryfallApi) -> Result<RgbImage> {
        let id = self.id()?;
        api.get_image_by_id(id, api::ImageFormat::PNG, api::ImageFace::Front)
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

    pub fn type_line(&self) -> Result<&str> {
        self.json
            .get("type_line")
            .ok_or_else(|| anyhow!("Card JSON missing \"type_line\" field"))?
            .as_str()
            .ok_or_else(|| anyhow!("Card JSON \"type_line\" field was not a string"))
    }

    pub fn oracle_text(&self) -> Result<&str> {
        self.json
            .get("oracle_text")
            .ok_or_else(|| anyhow!("Card JSON missing \"oracle_text\" field"))?
            .as_str()
            .ok_or_else(|| anyhow!("Card JSON \"oracle_text\" field is not a string"))
    }
}

pub async fn card_by_id<'db, 'a: 'db, DB: 'db>(
    db: &'a mut DB,
    id: ScryfallId,
) -> Result<ScryfallCard>
where
    &'a mut DB: Executor<'db, Database = Postgres>,
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

pub async fn oracle_id_by_name<'db, 'a: 'db, DB>(
    db: &'a mut DB,
    name: &str,
) -> Result<ScryfallOracleId>
where
    &'a mut DB: Executor<'db, Database = Postgres>,
{
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
            "Failed to get oracle ID for card with name {:?} from the database",
            name
        )
    })?;
    let oracle_id = row.get("oracle_id");
    Ok(ScryfallOracleId(oracle_id))
}

pub async fn load_bulk<P: AsRef<Path>>(
    api: &ScryfallApi,
    db: &mut PgConnection,
    root: P,
    force: bool,
) -> Result<()> {
    use async_std::fs;
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
    if download {
        debug!(
            "Cached file {} is out of date or non-existent, downloading fresh",
            cards_filename.to_string_lossy()
        );
        let bulk_file = api.get_bulk_data("default_cards").await?;
        let file = fs::File::create(&cards_filename).await?;
        async_std::io::copy(bulk_file, file).await?;
        debug!("Saved to cached file");
    }

    debug!("Loading cached file {}", cards_filename.to_string_lossy());

    struct ParseIter<'a> {
        pbar_bytes: pbr::ProgressBar<pbr::Pipe>,
        pbar_cards: pbr::ProgressBar<pbr::Pipe>,
        cursor: std::io::Cursor<&'a [u8]>,
        len: u64,
        seen_cards: u64,
        message_buffer: String,
        set_code_buffer: String,
    }
    impl<'a> ParseIter<'a> {
        fn new(bytes: &'a [u8], estimated_total: u64) -> Self {
            let len = bytes.len() as u64;
            let pbar_multi = pbr::MultiBar::new();
            let mut pbar_bytes = pbar_multi.create_bar(len);
            pbar_bytes.set_units(pbr::Units::Bytes);
            pbar_bytes.set_max_refresh_rate(Some(std::time::Duration::from_millis(100)));
            let mut pbar_cards = pbar_multi.create_bar(estimated_total);
            pbar_cards.set_max_refresh_rate(Some(std::time::Duration::from_millis(100)));
            pbar_cards.format(".....");
            pbar_cards.show_percent = false;
            pbar_cards.show_time_left = false;
            let cursor = std::io::Cursor::new(bytes);
            std::thread::spawn(move || {
                pbar_multi.listen();
            });
            Self {
                pbar_bytes,
                pbar_cards,
                cursor,
                len,
                seen_cards: 0,
                message_buffer: String::with_capacity(100),
                set_code_buffer: String::with_capacity(7),
            }
        }
    }

    impl<'a> Iterator for ParseIter<'a> {
        type Item = Result<Value>;

        fn next(&mut self) -> Option<Self::Item> {
            use serde::Deserialize;
            use std::io::Seek;

            macro_rules! some_error {
                ($result:expr) => {
                    match $result {
                        Ok(x) => x,
                        Err(e) => return Some(Err(e.into())),
                    }
                };
            }

            let position = some_error!(self.cursor.seek(std::io::SeekFrom::Current(1)));
            if (position + 1) >= self.len {
                self.pbar_cards.total = self.seen_cards;
                self.pbar_cards.finish();
                self.pbar_bytes.finish();
                return None;
            }

            let mut deserializer = serde_json::Deserializer::from_reader(&mut self.cursor);
            let value = some_error!(serde_json::Value::deserialize(&mut deserializer));
            self.pbar_bytes.set(self.cursor.position());
            let released_at = value
                .get("released_at")
                .and_then(Value::as_str)
                .unwrap_or("????-??-??");
            let set_code = value.get("set").and_then(Value::as_str).unwrap_or("???");
            let mut set_name = value
                .get("set_name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown set>");
            if set_name.len() > 30 {
                set_name = &set_name[0..30];
            }
            let mut name = value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown card>");
            if name.len() > 30 {
                name = &name[0..30];
            }
            self.message_buffer.clear();
            self.set_code_buffer.clear();
            {
                use std::fmt::Write;
                some_error!(write!(&mut self.set_code_buffer, "[{}]", set_code));
                some_error!(write!(
                    &mut self.message_buffer,
                    "{:>10} {:>7} {:<30} | {:<30} ",
                    released_at, self.set_code_buffer, set_name, name,
                ));
            }
            self.seen_cards += 1;
            if self.seen_cards > self.pbar_cards.total {
                self.pbar_cards.total += self.seen_cards;
            }
            self.pbar_cards.message(&self.message_buffer);
            self.pbar_cards.inc();
            Some(Ok(value))
        }
    }

    let mut file = fs::File::open(&cards_filename).await?;
    let expected_file_len = match file.metadata().await {
        Ok(m) => m.len() as usize,
        Err(_) => 10_000,
    };
    let mut bytes = Vec::with_capacity(expected_file_len);
    file.read_to_end(&mut bytes).await?;
    assert!(bytes.len() > 0, "Failed to fill bytes in load_bulk!");

    let start = std::time::Instant::now();
    let estimated_count = sqlx::query("SELECT COUNT(*) as total FROM scryfall_card;")
        .fetch_one(&mut *db)
        .await?
        .get::<i64, _>("total")
        .max(50_000) as u64;

    let cards_iter = ParseIter::new(bytes.as_slice(), estimated_count);
    info!("Saving cards from Scryfall into database...");
    for card_result in pbr::PbIter::new(cards_iter) {
        let card = card_result.context("Failed to load cards from Scryfall bulk data")?;
        ScryfallCard::save_from_json(&mut *db, card).await?;
    }
    let end = std::time::Instant::now();
    let std_delta = end - start;
    let trimmed_delta = std::time::Duration::from_secs(std_delta.as_secs());
    let delta = humantime::Duration::from(trimmed_delta);
    info!("Loading took {}", delta);

    Ok(())
}

// Expand a single oracle ID into multiple printed cards. We prefer full faced
// cards when available.
pub async fn expand_oracle_id(
    db: &mut PgConnection,
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
    .fetch(&mut *db);
    let mut rows: Vec<ScryfallCardRow> = vec![];
    while let Some(row_result) = rows_stream.next().await {
        let row = row_result?;
        rows.push(row);
    }
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
    db: impl Executor<'_, Database = Postgres>,
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

    fn add_bindings<'db>(
        self,
        mut query: sqlx::query::Query<'db, Postgres, PgArguments>,
    ) -> sqlx::query::Query<'db, Postgres, PgArguments> {
        for s in self.strings {
            query = query.bind::<String>(s.to_string());
        }
        query
    }
}

// trait BindableQuery<DB = Postgres>: Sized {
//     fn bind_value<T>(self, value: T) -> Self
//     where
//         DB: sqlx::types::HasSqlType<T>,
//         T: sqlx::encode::Encode<DB>;
// }

// impl<'q, DB: sqlx::Database> BindableQuery<DB> for sqlx::query::Query<'q, DB> {
//     fn bind_value<T>(self, value: T) -> Self
//     where
//         DB: sqlx::types::HasSqlType<T>,
//         T: sqlx::encode::Encode<DB>,
//     {
//         self.bind(value)
//     }
// }

// impl<'q, DB: sqlx::Database, R> BindableQuery<DB> for sqlx::query::QueryAs<'q, DB, R> {
//     fn bind_value<T>(self, value: T) -> Self
//     where
//         DB: sqlx::types::HasSqlType<T>,
//         T: sqlx::encode::Encode<DB>,
//     {
//         self.bind(value)
//     }
// }

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
    db: impl Executor<'_, Database = Postgres>,
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
        colors.insert(color);
    }
    Ok(colors)
}

pub async fn can_be_a_commander(
    db: impl Executor<'_, Database = Postgres>,
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
