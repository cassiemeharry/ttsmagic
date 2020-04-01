use async_trait::async_trait;
use failure::{format_err, Error};
use scraper::{Html, Selector};
use sqlx::{Executor, Postgres};
use std::{collections::HashMap, str::FromStr};
use url::Url;

use crate::{
    deck::{DeckLoader, ParsedDeck},
    scryfall::{api::ScryfallApi, ensure_card_by_name},
};

#[derive(Copy, Clone, Debug)]
pub(crate) struct DeckboxInfo {
    id: u32,
}

pub(crate) struct DeckboxLoader;

#[async_trait(?Send)]
impl DeckLoader for DeckboxLoader {
    type UrlInfo = DeckboxInfo;

    fn match_url(url: &Url) -> Option<DeckboxInfo> {
        match (url.domain(), url.path_segments()) {
            (Some("deckbox.org"), Some(path_segments)) => {
                let path_segments = path_segments.collect::<Vec<&str>>();
                match path_segments.as_slice() {
                    ["sets", raw_id] => {
                        let id = u32::from_str(raw_id).ok()?;
                        Some(DeckboxInfo { id })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    async fn parse_deck(
        api: &ScryfallApi,
        db: &mut impl Executor<Database = Postgres>,
        url_match: DeckboxInfo,
    ) -> Result<ParsedDeck, Error> {
        let title_selector = Selector::parse("span#deck_built_title").unwrap();
        let main_card_rows_selector = Selector::parse("table.set_cards.main tr[id]").unwrap();
        let sideboard_card_rows_selector =
            Selector::parse("table.set_cards.sideboard tr[id]").unwrap();
        let card_name_selector = Selector::parse("td.card_name").unwrap();
        let card_count_selector = Selector::parse("td.card_count").unwrap();

        let url = format!("https://deckbox.org/sets/{}", url_match.id);
        info!("Parsing Deckbox.org deck at {}", url);
        let client = surf::Client::new();
        let request = client.get(&url);
        let mut response = request.await.map_err(Error::from_boxed_compat)?;
        let html_string = response
            .body_string()
            .await
            .map_err(Error::from_boxed_compat)?;
        let html = Html::parse_document(&html_string);

        let title = {
            let mut matches = html.select(&title_selector).into_iter();
            let matched = matches
                .next()
                .ok_or_else(|| format_err!("No match for {:?}", title_selector))?;
            let text_sibling = matched
                .next_sibling()
                .ok_or_else(|| format_err!("No next sibling node for {:?}", title_selector))?
                .value()
                .as_text()
                .ok_or_else(|| {
                    format_err!(
                        "Next sibling of match for {:?} is not a text node",
                        title_selector
                    )
                })?;
            text_sibling.to_string().trim().to_string()
        };

        let mut main_deck = HashMap::with_capacity(110);
        let mut sideboard = HashMap::with_capacity(20);

        macro_rules! parse_section {
            ($selector:ident, $pile:ident) => {{
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
                            failure::format_err!(
                                "No card name found for Deckbox card with ID {} in deck {}",
                                card_id,
                                url_match.id
                            )
                        })?;
                    let card_name_parts: Vec<&str> = card_name_ref.text().collect();
                    let card_name = card_name_parts.join("");

                    let mut card_count: u8 = 0;
                    if let Some(card_count_ref) = row_ref.select(&card_count_selector).next() {
                        let card_count_text_parts: Vec<&str> = card_count_ref.text().collect();
                        let card_count_text = card_count_text_parts.join("");
                        match u8::from_str(&card_count_text) {
                            Ok(c) => card_count = c,
                            Err(_) => (),
                        };
                    }

                    let card = ensure_card_by_name(api, db, &card_name.trim()).await?;
                    let card_id = card.id()?;
                    let card_name = card.combined_name();
                    if let Some(_before) = $pile.insert(card_id, (card, card_count)) {
                        warn!("Found card {} multiple times!", card_name);
                    }
                }
            }};
        }

        parse_section!(main_card_rows_selector, main_deck);
        parse_section!(sideboard_card_rows_selector, sideboard);

        Ok(ParsedDeck {
            title,
            url,
            main_deck,
            sideboard,
        })
    }
}
