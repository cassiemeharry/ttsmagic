use anyhow::{anyhow, Context, Error, Result};
use async_trait::async_trait;
use redis::AsyncCommands;
use scraper::{Html, Selector};
use serde::Deserialize;
use sqlx::{Executor, Postgres};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};
use url::Url;

use crate::{
    deck::{Deck, DeckMatcher, DeckParser, UnparsedDeck},
    scryfall::{self, ScryfallOracleId},
};

async fn extract_commanders(
    db: &mut impl Executor<Database = Postgres>,
    main_deck: &mut HashMap<ScryfallOracleId, (String, u8)>,
) -> Result<HashMap<ScryfallOracleId, String>> {
    fn count_cards<'a>(cards: impl Iterator<Item = &'a (String, u8)>) -> usize {
        cards.map(|(_, count)| *count as usize).sum()
    }

    let mut commanders_pile: HashMap<ScryfallOracleId, String> = HashMap::new();
    if count_cards(main_deck.values()) != 100 {
        return Ok(commanders_pile);
    }
    for (oracle_id, (name, _count)) in main_deck.iter() {
        let legal_in_commander = scryfall::check_legality_by_oracle_id(db, *oracle_id, "commander")
            .await
            .with_context(|| {
                format!(
                    "Failed to check whether {} (oracle ID: {}) is legal in commander",
                    name, oracle_id
                )
            })?;
        if !legal_in_commander {
            debug!(
                "Card {} ({}) disqualified deck from commander format",
                name, oracle_id
            );
            return Ok(commanders_pile);
        }
    }
    let oracle_ids = main_deck.keys().copied().collect::<Vec<ScryfallOracleId>>();
    let deck_color_identity_owned = scryfall::deck_color_identity(db, oracle_ids.as_slice())
        .await
        .context("Failed to get deck color identity")?
        .into_iter()
        .collect::<Vec<String>>();
    let deck_color_identity_borrowed = deck_color_identity_owned
        .iter()
        .map(String::as_str)
        .collect::<Vec<&str>>();
    let deck_color_identity: &[&str] = deck_color_identity_borrowed.as_slice();
    debug!("Looks like a commander deck. Searching for the commander now...");
    // Dig out the commander and put it in its own pile.
    let mut commander_ids = HashSet::new();
    for (oracle_id, (name, count)) in main_deck.iter() {
        if *count != 1 {
            continue;
        }
        debug!(
            "Checking whether {} (oracle ID: {}) can be a commander with deck color identity {:?}...",
            name, oracle_id, deck_color_identity,
        );
        if scryfall::can_be_a_commander(db, *oracle_id, deck_color_identity)
            .await
            .with_context(|| {
                format!(
                    "Failed to check whether {} (oracle ID: {}) can be a commander",
                    name, oracle_id
                )
            })?
        {
            info!("Found potential commander {} ({})", name, oracle_id);
            commander_ids.insert(*oracle_id);
        }
    }
    for commander_id in commander_ids {
        let (name, _) = main_deck.remove(&commander_id).unwrap();
        commanders_pile.insert(commander_id, name);
    }
    Ok(commanders_pile)
}

fn get_text(elem_ref: scraper::ElementRef<'_>) -> String {
    let text_parts: Vec<&str> = elem_ref.text().collect();
    text_parts.join("").trim().to_string()
}

pub(crate) struct DeckboxLoader {
    id: u32,
}

impl DeckMatcher for DeckboxLoader {
    fn match_url(url: &Url) -> Option<Self> {
        match (url.domain(), url.path_segments()) {
            (Some("deckbox.org"), Some(path_segments)) => {
                let path_segments = path_segments.take(2).collect::<Vec<&str>>();
                match path_segments.as_slice() {
                    ["sets", raw_id] => {
                        let id = u32::from_str(raw_id).ok()?;
                        Some(DeckboxLoader { id })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

#[async_trait(?Send)]
impl<DB: Executor<Database = Postgres>, R: AsyncCommands> DeckParser<DB, R> for DeckboxLoader {
    fn name(&self) -> &'static str {
        "Deckbox"
    }

    fn canonical_deck_url(&self) -> Url {
        let id_str = format!("{}", self.id);
        let mut url = Url::parse(&format!("https://deckbox.org/")).unwrap();
        url.path_segments_mut()
            .unwrap()
            .extend(&["sets", id_str.as_str(), ""]);
        url
    }

    async fn parse_deck(&self, db: &mut DB, redis: &mut R, unparsed: UnparsedDeck) -> Result<Deck> {
        let title_selector = Selector::parse(".page_header > .section_title > span").unwrap();
        let main_card_rows_selector = Selector::parse("table.set_cards.main tr[id]").unwrap();
        let sideboard_card_rows_selector =
            Selector::parse("table.set_cards.sideboard tr[id]").unwrap();
        let card_name_selector = Selector::parse("td.card_name").unwrap();
        let card_count_selector = Selector::parse("td.card_count").unwrap();

        let url = format!("https://deckbox.org/sets/{}", self.id);
        info!("Parsing Deckbox.org deck at {}", url);
        let client = surf::Client::new();
        let request = client.get(&url);
        let mut response = request.await.map_err(Error::msg)?;
        let html_string = response.body_string().await.map_err(Error::msg)?;
        let html = Html::parse_document(&html_string);
        debug!("Got {:?} bytes of HTML", html_string.len());

        let title = {
            let mut matches = html.select(&title_selector).into_iter();
            let matched = matches
                .next()
                .ok_or_else(|| anyhow!("No match for {:?}", title_selector))?;
            let s = get_text(matched);
            anyhow::ensure!(
                !s.is_empty(),
                "Found empty string where deck title was expected!"
            );
            s
        };

        let mut main_deck = HashMap::with_capacity(110);
        let mut sideboard = HashMap::with_capacity(20);

        macro_rules! parse_section {
            ($self:ident, $selector:ident, $pile:ident) => {{
                for row_ref in html.select(&$selector) {
                    let row = row_ref.value();
                    let card_id = match row.attr("id") {
                        None => continue,
                        Some(row_id_str) => match u64::from_str(row_id_str) {
                            Ok(row_id) => row_id,
                            Err(_) => continue,
                        },
                    };
                    let card_name_ref =
                        row_ref.select(&card_name_selector).next().ok_or_else(|| {
                            anyhow!(
                                "No card name found for Deckbox card with ID {} in deck {}",
                                card_id,
                                $self.id,
                            )
                        })?;
                    let card_name = get_text(card_name_ref);

                    let mut card_count: u8 = 0;
                    if let Some(card_count_ref) = row_ref.select(&card_count_selector).next() {
                        let card_count_str = get_text(card_count_ref);
                        match u8::from_str(&card_count_str) {
                            Ok(c) => card_count = c,
                            Err(_) => (),
                        };
                    }

                    let card_name = card_name.trim();
                    debug!("Looking up oracle ID for Deckbox card {:?}", card_name);
                    let oracle_id = scryfall::oracle_id_by_name(db, card_name).await?;
                    if let Some(_before) =
                        $pile.insert(oracle_id, (card_name.to_string(), card_count))
                    {
                        warn!("Found card {} multiple times!", card_name);
                    }
                }
            }};
        }

        parse_section!(self, main_card_rows_selector, main_deck);
        parse_section!(self, sideboard_card_rows_selector, sideboard);
        let commanders = extract_commanders(db, &mut main_deck)
            .await
            .context("Failed to extract commanders from main deck list")?;

        let deck = unparsed
            .save_cards(db, redis, title, commanders, main_deck, sideboard)
            .await?;

        Ok(deck)
    }
}

pub(crate) struct TappedOutLoader {
    slug: String,
}

impl DeckMatcher for TappedOutLoader {
    fn match_url(url: &Url) -> Option<Self> {
        match (url.domain(), url.path_segments()) {
            (Some("tappedout.net"), Some(path_segments)) => {
                let path_segments = path_segments.take(2).collect::<Vec<&str>>();
                let slug = match path_segments.as_slice() {
                    ["mtg-decks", slug] => slug.to_string(),
                    _ => return None,
                };
                Some(TappedOutLoader { slug })
            }
            _ => None,
        }
    }
}

#[async_trait(?Send)]
impl<DB: Executor<Database = Postgres>, R: AsyncCommands> DeckParser<DB, R> for TappedOutLoader {
    fn name(&self) -> &'static str {
        "TappedOut"
    }

    fn canonical_deck_url(&self) -> Url {
        let mut url = Url::parse(&format!("https://tappedout.net/")).unwrap();
        url.path_segments_mut()
            .unwrap()
            .extend(&["mtg-decks", self.slug.as_str(), ""]);
        url
    }

    async fn parse_deck(&self, db: &mut DB, redis: &mut R, unparsed: UnparsedDeck) -> Result<Deck> {
        #[derive(Deserialize)]
        struct TappedOutCSVRow {
            #[serde(rename = "Board")]
            board: String,
            #[serde(rename = "Qty")]
            count: u8,
            #[serde(rename = "Name")]
            name: String,
            #[serde(default, rename = "Commander")]
            commander_col: String,
        }

        let url = format!("https://tappedout.net/mtg-decks/{}/", self.slug);
        let csv_url = format!("{}?fmt=csv", url);
        let client = surf::Client::new();
        info!("Parsing TappedOut.org deck at {}", url);

        let title = {
            let request = client
                .get(&url)
                .middleware(crate::utils::SurfRedirectMiddleware::new());
            let mut response = request
                .await
                .map_err(Error::msg)
                .context("Failed to load deck page from TappedOut")?;
            let html_string = &response
                .body_string()
                .await
                .map_err(Error::msg)
                .context("Failed to load contents of deck page from TappedOut")?;
            let html = Html::parse_document(&html_string);
            let title_selector = Selector::parse(".well.well-jumbotron h2").unwrap();
            let mut matches = html.select(&title_selector);
            let matched = matches
                .next()
                .ok_or(anyhow!("Failed to find title for TappedOut deck"))?;
            get_text(matched)
        };

        let mut commanders = HashMap::new();
        let mut main_deck = HashMap::with_capacity(110);
        let mut sideboard = HashMap::new();

        let request = client
            .get(&csv_url)
            .middleware(crate::utils::SurfRedirectMiddleware::new());
        let mut response = request.await.map_err(Error::msg)?;
        let csv_bytes = response.body_bytes().await.map_err(Error::msg)?;
        let csv_cursor = std::io::Cursor::new(csv_bytes);
        let mut csv_reader = csv::Reader::from_reader(csv_cursor);

        for row_result in csv_reader.deserialize::<TappedOutCSVRow>() {
            let mut row = row_result
                .with_context(|| format!("Failed to parse TappedOut deck CSV from {}", url))?;

            // TappedOut sometimes renders split card names with a single slash,
            // while we canonicalize them with two slashes.
            if row.name.contains(" / ") {
                row.name = row.name.replace(" / ", " // ");
            }

            let oracle_id = scryfall::oracle_id_by_name(db, &row.name)
                .await
                .with_context(|| format!("Failed to load TappedOut deck {}", self.slug))?;
            if row.commander_col == "True" {
                commanders.insert(oracle_id, row.name.to_string());
                continue;
            }
            let pile = match row.board.as_str() {
                "main" => &mut main_deck,
                "maybe" => {
                    debug!(
                        "Skipping \"maybe\" row in TappedOut deck for card {}",
                        row.name
                    );
                    continue;
                }
                "acquire" => {
                    debug!(
                        "Skipping \"acquire\" row in TappedOut deck for card {}",
                        row.name
                    );
                    continue;
                }
                "side" => &mut sideboard,
                other => {
                    warn!(
                        "Unexpected TappedOut \"board\" value for card {}: {:?}",
                        row.name, other
                    );
                    continue;
                }
            };
            let entry = pile
                .entry(oracle_id)
                .or_insert_with(|| (row.name.to_string(), 0));
            entry.1 += row.count;
        }

        let deck = unparsed
            .save_cards(db, redis, title, commanders, main_deck, sideboard)
            .await?;
        Ok(deck)
    }
}

pub(crate) struct ArchidektLoader {
    id: u64,
}

impl DeckMatcher for ArchidektLoader {
    fn match_url(url: &Url) -> Option<Self> {
        match (url.domain(), url.path_segments()) {
            (Some("archidekt.com"), Some(path_segments))
            | (Some("www.archidekt.com"), Some(path_segments)) => {
                let path_segments = path_segments.take(2).collect::<Vec<&str>>();
                let id = match path_segments.as_slice() {
                    ["decks", id_str] => id_str.parse().ok()?,
                    _ => return None,
                };
                Some(ArchidektLoader { id })
            }
            _ => None,
        }
    }
}

#[async_trait(?Send)]
impl<DB: Executor<Database = Postgres>, R: AsyncCommands> DeckParser<DB, R> for ArchidektLoader {
    fn name(&self) -> &'static str {
        "Archidekt"
    }

    fn canonical_deck_url(&self) -> Url {
        let url_string = format!("https://archidekt.com/decks/{}", self.id);
        Url::parse(&url_string).unwrap()
    }

    async fn parse_deck(&self, db: &mut DB, redis: &mut R, unparsed: UnparsedDeck) -> Result<Deck> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ArchidektResponse {
            id: u64,
            name: String,
            cards: Vec<ArchidektResponseCardWrapper>,
            categories: Vec<ArchidektResponseCategory>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ArchidektResponseCardWrapper {
            card: ArchidektResponseCard,
            quantity: u8,
            categories: Vec<String>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ArchidektResponseCard {
            oracle_card: ArchidektResponseOracleCard,
            uid: uuid::Uuid,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ArchidektResponseOracleCard {
            name: String,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ArchidektResponseCategory {
            name: String,
            included_in_deck: bool,
            // is_premier: bool,
        }

        let json_url = format!("https://archidekt.com/api/decks/{}/small/", self.id);
        let client = surf::Client::new();
        info!("Parsing Archidekt deck at {}", json_url);

        let request = client
            .get(&json_url)
            .middleware(crate::utils::SurfRedirectMiddleware::new());
        let mut response = request
            .await
            .map_err(Error::msg)
            .context("Failed to load deck JSON from Archidekt")?;
        // let response_string = response
        //     .body_string()
        //     .await
        //     .map_err(Error::msg)
        //     .context("Failed to get response body from Archidekt as a String")?;
        // debug!("Archidekt response JSON: {:?}", response_string);
        let response_value = response
            .body_json::<ArchidektResponse>()
            .await
            .map_err(Error::msg)
            .context("Failed to parse deck JSON from Archidekt")?;

        if response_value.id != self.id {
            return Err(anyhow!("Archidekt API returned a different deck than we asked for! Got {:?}, expected {:?}", response_value.id, self.id));
        }

        let title = response_value.name;

        let mut commanders = HashMap::new();
        let mut main_deck = HashMap::with_capacity(110);
        let mut sideboard = HashMap::new();

        struct ArchidektCategoryInfo {
            included_in_deck: bool,
        }

        let categories = {
            let mut cs = HashMap::new();
            for category in response_value.categories {
                cs.insert(
                    category.name.clone(),
                    ArchidektCategoryInfo {
                        included_in_deck: category.included_in_deck,
                        // is_premier: category.is_premier,
                    },
                );
            }
            cs
        };

        for card_wrapper in response_value.cards {
            let card_name = card_wrapper.card.oracle_card.name;
            let card_id = card_wrapper.card.uid.into();
            let oracle_id = {
                let raw_card_result = scryfall::card_by_id(db, card_id).await;
                match raw_card_result {
                    Ok(card) => card.oracle_id().with_context(|| {
                        format!("Failed to get Oracle ID for card {}", card_name.clone())
                    })?,
                    Err(_) => scryfall::oracle_id_by_name(db, &card_name)
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to find a card named {:?} for Archidekt deck {:?}",
                                card_name, self.id
                            )
                        })?,
                }
            };

            let is_commander = card_wrapper.categories.iter().any(|c| c == "Commander");
            let is_sideboard = card_wrapper
                .categories
                .iter()
                .any(|c| match categories.get(c) {
                    None => false,
                    Some(cat) => !cat.included_in_deck,
                });
            if is_commander {
                commanders.insert(oracle_id, card_name);
            } else if is_sideboard {
                sideboard.insert(oracle_id, (card_name, card_wrapper.quantity));
            } else {
                main_deck.insert(oracle_id, (card_name, card_wrapper.quantity));
            }
        }

        let deck = unparsed
            .save_cards(db, redis, title, commanders, main_deck, sideboard)
            .await?;
        Ok(deck)
    }
}
