use anyhow::{anyhow, Context, Error, Result};
use async_trait::async_trait;
use scraper::{Html, Selector};
use serde::Deserialize;
use sqlx::{Executor, Postgres};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};
use url::Url;

use crate::{
    deck::{DeckLoader, ParsedDeck},
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
                    "Checking whether {} (oracle ID: {}) is legal in commander",
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
        .context("Getting deck color identity")?
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
                    "Checking whether {} (oracle ID: {}) can be a commander",
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

#[async_trait(?Send)]
impl DeckLoader for DeckboxLoader {
    fn match_url(url: &Url) -> Option<Self> {
        match (url.domain(), url.path_segments()) {
            (Some("deckbox.org"), Some(path_segments)) => {
                let path_segments = path_segments.collect::<Vec<&str>>();
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

    async fn parse_deck(&self, db: &mut impl Executor<Database = Postgres>) -> Result<ParsedDeck> {
        let title_selector = Selector::parse("span#deck_built_title").unwrap();
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

        let title = {
            let mut matches = html.select(&title_selector).into_iter();
            let matched = matches
                .next()
                .ok_or_else(|| anyhow!("No match for {:?}", title_selector))?;
            let text_sibling = matched
                .next_sibling()
                .ok_or_else(|| anyhow!("No next sibling node for {:?}", title_selector))?
                .value()
                .as_text()
                .ok_or_else(|| {
                    anyhow!(
                        "Next sibling of match for {:?} is not a text node",
                        title_selector
                    )
                })?;
            text_sibling.to_string().trim().to_string()
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

                    let oracle_id = scryfall::oracle_id_by_name(db, &card_name.trim()).await?;
                    if let Some(_before) =
                        $pile.insert(oracle_id, (card_name.trim().to_string(), card_count))
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
            .context("Extracting commanders from main deck list")?;

        Ok(ParsedDeck {
            title,
            url,
            commanders,
            main_deck,
            sideboard,
        })
    }
}

pub(crate) struct TappedOutLoader {
    slug: String,
}

#[async_trait(?Send)]
impl DeckLoader for TappedOutLoader {
    fn match_url(url: &Url) -> Option<Self> {
        match (url.domain(), url.path_segments()) {
            (Some("tappedout.net"), Some(path_segments)) => {
                let path_segments = path_segments.take(2).collect::<Vec<&str>>();
                debug!("Found a tappedout URL, path segments: {:?}", path_segments);
                let slug = match path_segments.as_slice() {
                    ["mtg-decks", slug] => slug.to_string(),
                    _ => return None,
                };
                Some(TappedOutLoader { slug })
            }
            _ => None,
        }
    }

    async fn parse_deck(&self, db: &mut impl Executor<Database = Postgres>) -> Result<ParsedDeck> {
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
            let request = client.get(&url);
            let mut response = request.await.map_err(Error::msg)?;
            let html_string = response.body_string().await.map_err(Error::msg)?;
            let html = Html::parse_document(&html_string);
            let title_selector = Selector::parse(".well.well-jumbotron h2").unwrap();
            let mut matches = html.select(&title_selector);
            let matched = matches
                .next()
                .ok_or_else(|| anyhow!("No match for {:?}", title_selector))?;
            get_text(matched)
        };

        let mut commanders = HashMap::new();
        let mut main_deck = HashMap::with_capacity(110);
        let mut sideboard = HashMap::new();

        let request = client.get(&csv_url);
        let mut response = request.await.map_err(Error::msg)?;
        let csv_bytes = response.body_bytes().await.map_err(Error::msg)?;
        let csv_cursor = std::io::Cursor::new(csv_bytes);
        let mut csv_reader = csv::Reader::from_reader(csv_cursor);

        for row_result in csv_reader.deserialize::<TappedOutCSVRow>() {
            let row =
                row_result.with_context(|| format!("Parsing TappedOut deck CSV from {}", url))?;
            let oracle_id = scryfall::oracle_id_by_name(db, &row.name)
                .await
                .with_context(|| format!("Loading TappedOut deck {}", self.slug))?;
            if row.commander_col == "True" {
                commanders.insert(oracle_id, row.name.to_string());
                continue;
            }
            let pile = match row.board.as_str() {
                "main" => &mut main_deck,
                "maybe" => &mut sideboard,
                "acquire" => {
                    debug!(
                        "Skipping \"acquire\" row in TappedOut deck for card {}",
                        row.name
                    );
                    continue;
                }
                other => Err(anyhow!(
                    "Unexpected TappedOut \"board\" value for card {}: {:?}",
                    row.name,
                    other
                ))?,
            };
            let entry = pile
                .entry(oracle_id)
                .or_insert_with(|| (row.name.to_string(), 0));
            entry.1 += row.count;
        }

        Ok(ParsedDeck {
            title,
            url,
            commanders,
            main_deck,
            sideboard,
        })
    }
}
