use anyhow::{anyhow, Context, Result};
use async_std::{prelude::*, sync::Arc};
use chrono::prelude::*;
use futures::future::BoxFuture;
use image::{imageops, RgbImage};
use redis::AsyncCommands;
use serde_json::{json, Value};
use smallvec::SmallVec;
use sqlx::{Executor, Postgres};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    convert::{TryFrom, TryInto},
    num::NonZeroU16,
    str::FromStr,
};
use ttsmagic_types::server_to_frontend as s2f;

mod render_coordinator;
use render_coordinator::wait_for_lock;

use crate::{
    deck::Deck,
    files::{MediaFile, StaticFiles},
    notify::notify_user,
    scryfall::{self, api::ScryfallApi, ScryfallCard, ScryfallId, ScryfallOracleId},
    utils::AsyncParallelStream,
};

#[derive(Clone, Debug)]
pub struct RenderedDeck {
    pub json_description: Value,
    pub rendered_at: DateTime<Utc>,
    pub pages: Vec<RenderedPage>,
}

#[derive(Clone, Debug)]
pub struct RenderedPage {
    width: u32,
    height: u32,
    pub image: MediaFile,
    card_mapping: HashMap<ScryfallId, u8>,
}

fn get_parts(card: ScryfallCard) -> Box<dyn Iterator<Item = ScryfallId> + Send> {
    let all_parts: Vec<Value> = match card.raw_json().get("all_parts").map(Value::as_array) {
        None => return Box::new(std::iter::empty()),
        Some(None) => return Box::new(std::iter::empty()),
        Some(Some(all_parts)) => all_parts.clone(),
    };
    let card_name = card.combined_name();
    let iter = all_parts.into_iter().enumerate().filter_map(move |(i, part_json)| {
        let raw_part_id_value = match part_json.get("id") {
            Some(raw_id) => raw_id,
            None => {
                warn!(
                    "Related card object {} (for card {}) is missing its \"id\" field.",
                    i, card_name,
                );
                return None;
            }
        };
        let raw_part_id = match raw_part_id_value.as_str() {
            Some(s) => s,
            None => {
                warn!(
                    "Related card object {} (for card {}) \"id\" field is not a string.",
                    i,
                    card_name
                );
                return None;
            }
        };
        let part_id = match ScryfallId::from_str(raw_part_id) {
            Ok(id) => id,
            Err(_) => {
                warn!("Related card object {} (for card {})'s \"id\" field is not a valid card ID.", i, card_name);
                return None;
            }
        };
        let component_type = match part_json.get("component") {
                None => {
                    warn!(
                        "Missing \"component\" field on related part for card {}",
                        card_name
                    );
                    return None;
                }
                Some(c_value) => match c_value.as_str() {
                    Some(c_value_str) => c_value_str,
                    None => {
                        warn!(
                            "Related card {} (for card {}) \"component\" field is not a string.",
                            part_id,
                            card_name
                        );
                        return None;
                    }
                }
        };
        match component_type {
            "combo_piece" => {
                debug!(
                    "Found combo piece {} related to card {}",
                    part_id, card_name
                );
                return None;
            }
            "meld_part" => {
                debug!("Found meld part {} related to card {}", part_id, card_name);
                return None;
            }
            "meld_result" => debug!(
                "Found meld result {} related to card {}",
                part_id, card_name
            ),
            "token" => debug!("Found token {} related to card {}", part_id, card_name),
            other => {
                warn!(
                    "Found unexpected related card component type {:?} for {} related to card {}",
                    other, part_id, card_name
                );
                return None;
            }
        };
        Some(part_id)
    });
    Box::new(iter)
}

async fn get_tokens(
    db: &mut impl Executor<Database = Postgres>,
    cards: impl Iterator<Item = ScryfallCard>,
) -> Result<Vec<ScryfallCard>> {
    let (size_hint, _) = cards.size_hint();
    let mut work_queue = VecDeque::with_capacity(size_hint);
    for card in cards {
        work_queue.push_back(card);
    }
    let mut seen_ids = HashSet::with_capacity(work_queue.len());
    let mut parts: HashMap<ScryfallOracleId, ScryfallCard> =
        HashMap::with_capacity(work_queue.len());

    while let Some(card) = work_queue.pop_front() {
        if !seen_ids.insert(card.id()?) {
            // We've seen this card before, don't reprocess it.
            continue;
        }
        let card_name = card.combined_name();
        for part_id in get_parts(card.clone()) {
            if seen_ids.contains(&part_id) {
                debug!("Already seen part {}", part_id);
                continue;
            }
            let part_card = scryfall::card_by_id(db, part_id).await.with_context(|| {
                format!("Failed to get related card {} for {}", part_id, card_name)
            })?;
            let part_oracle_id = part_card.oracle_id()?;
            work_queue.push_back(part_card.clone());
            parts.entry(part_oracle_id).or_insert(part_card);
        }
    }
    let mut tokens: Vec<ScryfallCard> = parts.into_iter().map(|(_k, v)| v).collect();
    tokens.sort_by_key(|c| c.combined_name());
    Ok(tokens)
}

#[derive(Clone, Debug)]
struct Pile {
    cards: Vec<(ScryfallCard, u8)>,
    face_up: bool,
}

impl Pile {
    fn new_face_up(cards: Vec<(ScryfallCard, u8)>) -> Result<Self> {
        if cards.is_empty() {
            Err(anyhow!("Cannot make a pile of zero cards"))
        } else {
            Ok(Pile {
                cards,
                face_up: true,
            })
        }
    }

    fn new_face_down(cards: Vec<(ScryfallCard, u8)>) -> Result<Self> {
        if cards.is_empty() {
            Err(anyhow!("Cannot make a pile of zero cards"))
        } else {
            Ok(Pile {
                cards,
                face_up: false,
            })
        }
    }
}

#[derive(Clone, Debug)]
struct LinearPile {
    cards: Vec<(ScryfallCard, u16)>,
    face_up: bool,
}

impl TryFrom<(Pile, &'_ [RenderedPage])> for LinearPile {
    type Error = anyhow::Error;

    fn try_from((pile, pages): (Pile, &'_ [RenderedPage])) -> Result<LinearPile, Self::Error> {
        let mut cards = Vec::with_capacity(pile.cards.len());
        let card_id_to_deck_id = {
            let mut mapping = HashMap::new();
            for (i, page) in pages.iter().enumerate() {
                for (card_id, page_index) in page.card_mapping.iter() {
                    let deck_id: u16 = (100 * ((i as u16) + 1)) + (*page_index as u16);
                    mapping.insert(*card_id, deck_id);
                }
            }
            mapping
        };
        for (card, count) in pile.cards {
            let card_id = card.id()?;
            let deck_id = match card_id_to_deck_id.get(&card_id) {
                Some(deck_id) => *deck_id,
                None => return Err(anyhow!("Card {} not found in pages", card_id)),
            };
            for _ in 0..count {
                cards.push((card.clone(), deck_id));
            }
        }
        Ok(LinearPile {
            cards,
            face_up: pile.face_up,
        })
    }
}

type Piles = SmallVec<[Pile; 4]>;

async fn collect_card_piles(
    db: &mut impl Executor<Database = Postgres>,
    deck: &Deck,
) -> Result<Piles> {
    let deck_url = deck.url.clone();

    let commanders_pile = {
        let mut pile = Vec::with_capacity(deck.commanders.len());
        for card in deck.commanders.values().cloned() {
            pile.push((card, 1));
            pile.sort_by_key(|(c, _)| c.combined_name());
        }
        pile
    };

    let main_deck = {
        let mut pile = Vec::with_capacity(deck.main_deck.len());
        for (_, (card, count)) in deck.main_deck.iter() {
            pile.push((card.clone(), count.clone()));
        }
        pile.sort_by_key(|(c, _)| c.combined_name());
        pile
    };
    debug!(
        "Found main deck: {:?}",
        main_deck
            .iter()
            .map(|(c, count)| format!("{}x {}", count, c.combined_name()))
            .collect::<Vec<_>>(),
    );
    if main_deck.is_empty() {
        return Err(anyhow!(
            "Tried to collect an empty deck (from URL: {})",
            deck_url
        ));
    }

    let sideboard = {
        let mut pile = Vec::with_capacity(deck.sideboard.len());
        for (_, (card, count)) in deck.sideboard.iter() {
            pile.push((card.clone(), count.clone()));
        }
        pile.sort_by_key(|(c, _)| c.combined_name());
        pile
    };
    debug!(
        "Found sideboard: {:?}",
        sideboard
            .iter()
            .map(|(c, count)| format!("{}x {}", count, c.combined_name()))
            .collect::<Vec<_>>(),
    );
    let tokens = get_tokens(
        db,
        main_deck
            .iter()
            .chain(sideboard.iter())
            .map(|(c, _)| c.clone()),
    )
    .await
    .with_context(|| format!("Failed to get tokens for deck {}", deck_url))?;
    let tokens: Vec<_> = tokens.into_iter().map(|t| (t, 1)).collect();
    if !tokens.is_empty() {
        debug!(
            "Found tokens: {:?}",
            tokens
                .iter()
                .map(|(c, _)| c.combined_name())
                .collect::<Vec<_>>()
        );
    }
    let mut piles = SmallVec::new();

    if !commanders_pile.is_empty() {
        piles.push(Pile::new_face_up(commanders_pile)?);
    }
    assert!(!main_deck.is_empty()); // checked earlier
    piles.push(Pile::new_face_down(main_deck)?);
    if !sideboard.is_empty() {
        piles.push(Pile::new_face_up(sideboard)?);
    }
    if !tokens.is_empty() {
        piles.push(Pile::new_face_up(tokens)?);
    }

    Ok(piles)
}

struct Page {
    width: u32,
    height: u32,
    image: RgbImage,
    card_mapping: HashMap<ScryfallId, u8>,
}

impl Page {
    async fn new(expected_cards: usize) -> Result<Self> {
        let expected_cards: u32 = expected_cards.try_into()?;
        const VALID_WIDTH_HEIGHTS: &[(u32, u32)] = &[
            (2, 2),
            (3, 2),
            (4, 3),
            (5, 4),
            (6, 4),
            (7, 5),
            (8, 6),
            (9, 6),
        ];
        let mut size: Option<(u32, u32)> = None;
        for (w, h) in VALID_WIDTH_HEIGHTS.iter().copied() {
            if expected_cards < ((w * h) - 1) {
                size = Some((w, h));
                break;
            }
        }
        let (width, height) = size.unwrap_or((10, 7));
        let page = Page {
            width,
            height,
            image: new_blank_page(width, height).await?,
            card_mapping: HashMap::new(),
        };
        Ok(page)
    }
}

// format=large: (672, 936)
// format=png:   (745, 1040)
const CARD_WIDTH: u32 = 672;
const CARD_HEIGHT: u32 = 936;

async fn fixup_size(image: RgbImage) -> RgbImage {
    const CARD_SIZE: (u32, u32) = (CARD_WIDTH, CARD_HEIGHT);
    if image.dimensions() == CARD_SIZE {
        image
    } else {
        let resized = async_std::task::spawn_blocking(move || {
            debug!(
                "Resizing a card image from {:?} to {:?}",
                image.dimensions(),
                CARD_SIZE
            );
            imageops::resize(
                &image,
                CARD_WIDTH,
                CARD_HEIGHT,
                imageops::FilterType::Lanczos3,
            )
        });
        resized.await
    }
}

async fn add_to_page(mut page: RgbImage, card: RgbImage, row: u32, column: u32) -> RgbImage {
    let added = async_std::task::spawn_blocking(move || {
        let card = async_std::task::block_on(fixup_size(card));
        imageops::overlay(&mut page, &card, column * CARD_WIDTH, row * CARD_HEIGHT);
        page
    });
    added.await
}

lazy_static::lazy_static! {
    static ref HIDDEN_IMAGE: RgbImage = {
        debug!("Loading hidden image");
        let bytes = StaticFiles::get("ttsmagic_hidden_face.png")
            .expect("ttsmagic_hidden_face.png is missing from static folder");
        let image = image::load_from_memory(&bytes).expect("Failed to load static file ttsmagic_hidden_face.png").to_rgb();
        async_std::task::block_on(fixup_size(image))
    };
}

async fn new_blank_page(cards_wide: u32, cards_high: u32) -> Result<RgbImage> {
    debug!(
        "Creating new blank page image ({} cards across and {} cards tall, {}x{} pixels)",
        cards_wide,
        cards_high,
        CARD_WIDTH * cards_wide,
        CARD_HEIGHT * cards_high
    );
    let page = RgbImage::new(CARD_WIDTH * cards_wide, CARD_HEIGHT * cards_high);
    let row = cards_high - 1;
    let column = cards_wide - 1;
    let page = add_to_page(page, HIDDEN_IMAGE.clone(), row, column).await;
    Ok(page)
}

async fn make_pages<R: AsyncCommands>(
    api: Arc<ScryfallApi>,
    redis: &mut R,
    deck: &Deck,
    piles: Arc<Piles>,
) -> Result<Vec<Page>> {
    // The higher the parallelism, the faster the pages can be rendered, but the
    // more memory this function will consume.
    const PARALLELISM: usize = 10;

    let mut page_images = Vec::with_capacity(piles.len());
    let mut current_page = Page::new(piles.iter().map(|p| p.cards.len()).sum()).await?;
    // These futures are `spawn`ed, which means they will be evaluated in
    // parallel. This works out to be *much* faster than loading them serially,
    // though it does take more memory.
    let mut card_load_futures: Vec<BoxFuture<'static, Result<(ScryfallCard, RgbImage)>>> = vec![];
    for pile in piles.iter() {
        for (card, _count) in pile.cards.iter() {
            let task_card = card.clone();
            let api = Arc::clone(&api);
            let wrapper_card = card.clone();
            let card_name = card.combined_name();
            let future = async move {
                debug!("Loading card {}...", card_name);
                let image = task_card.ensure_image(&api).await?;
                let image = fixup_size(image).await;
                debug!("Finished loading card {}", card_name);
                Ok((wrapper_card, image))
            };
            let future = Box::pin(future) as BoxFuture<_>;
            card_load_futures.push(future);
        }
    }
    let image_count = NonZeroU16::new(card_load_futures.len().try_into()?)
        .ok_or_else(|| anyhow!("Tried to render a deck with no cards in it"))?;
    let mut card_load_stream = AsyncParallelStream::new(PARALLELISM, card_load_futures);
    let mut images_rendered: u16 = 0;
    notify_user(
        redis,
        deck.user_id,
        s2f::Notification::RenderProgress {
            deck_id: deck.id,
            progress: s2f::RenderProgress::RenderingImages {
                rendered_cards: 0,
                total_cards: image_count,
            },
        },
    )
    .await?;
    let mut k = 0;
    while let Some(card_info) = card_load_stream.next().await {
        let (card, image) = card_info.context("Failed to load card image")?;
        let card_id = card.id()?;
        if (current_page.card_mapping.len() as u32)
            >= ((current_page.width * current_page.height) - 1)
        {
            debug!(
                "Finalizing page {} and starting another.",
                page_images.len()
            );
            page_images.push(current_page);
            current_page = Page::new((image_count.get() as usize) - k - 1).await?;
        }
        let page_index: u32 = current_page.card_mapping.len() as u32;
        let cards_per_page = (current_page.width * current_page.height) - 1;
        let row = (page_index % cards_per_page) / current_page.width;
        let column = (page_index % cards_per_page) % current_page.width;
        assert!(row < current_page.height);
        assert!(column < current_page.width);
        debug!(
            "Placing card #{} of {}, {} ({}) on page {} at row {}, column {}",
            k + 1,
            image_count,
            card.combined_name(),
            card_id,
            page_images.len(),
            row,
            column
        );
        current_page.image = add_to_page(current_page.image, image, row, column).await;
        current_page
            .card_mapping
            .insert(card_id, page_index.try_into().unwrap());
        images_rendered += 1;

        notify_user(
            redis,
            deck.user_id,
            s2f::Notification::RenderProgress {
                deck_id: deck.id,
                progress: s2f::RenderProgress::RenderingImages {
                    rendered_cards: images_rendered,
                    total_cards: image_count,
                },
            },
        )
        .await?;
        k += 1;
    }
    if !current_page.card_mapping.is_empty() {
        page_images.push(current_page);
    }

    Ok(page_images)
}

async fn save_pages<R: AsyncCommands>(
    redis: &mut R,
    deck: &Deck,
    pages: Vec<Page>,
) -> Result<Vec<RenderedPage>> {
    let total_pages = NonZeroU16::new(pages.len().try_into()?)
        .ok_or_else(|| anyhow!("Tried to save zero pages"))?;
    notify_user(
        redis,
        deck.user_id,
        s2f::Notification::RenderProgress {
            deck_id: deck.id,
            progress: s2f::RenderProgress::SavingPages {
                saved_pages: 0,
                total_pages,
            },
        },
    )
    .await?;

    let mut saved_pages = Vec::with_capacity(pages.len());
    for (i, page) in pages.into_iter().enumerate() {
        let deck_uuid = format!("{}", deck.id.as_uuid());
        let page_filename = format!(
            "pages/{}/{}/{}_{}.jpg",
            &deck_uuid[0..2],
            &deck_uuid[2..4],
            deck_uuid,
            i,
        );
        let f = MediaFile::create(&page_filename).await?;
        page.image.save(&f.path())?;
        let saved = f.finalize().await?;
        debug!("Saved page image {}", saved.path());
        saved_pages.push(RenderedPage {
            width: page.width,
            height: page.height,
            image: saved,
            card_mapping: page.card_mapping,
        });
        notify_user(
            redis,
            deck.user_id,
            s2f::Notification::RenderProgress {
                deck_id: deck.id,
                progress: s2f::RenderProgress::SavingPages {
                    saved_pages: (i + 1).try_into()?,
                    total_pages,
                },
            },
        )
        .await?;
    }

    Ok(saved_pages)
}

fn render_piles_to_json<'a>(
    deck_title: &str,
    piles: Piles,
    pages: &'a [RenderedPage],
) -> Result<Value> {
    let base_transform = json!({
        "posX": 0.0,
        "posY": 0.0,
        "posZ": 0.0,
        "rotX": 0.0,
        "rotY": 180.0,
        "rotZ": 0.0,
        "scaleX": 1.0,
        "scaleY": 1.0,
        "scaleZ": 1.0,
    });
    let color = json!({"r": 1.0, "g": 1.0, "b": 1.0});
    let decks_json_objs = {
        let back_url = "https://ttsmagic.cards/files/card_data/backing.jpg";
        let mut decks = Vec::with_capacity(pages.len());
        for page in pages.iter() {
            let face_url = page.image.url()?.to_string();
            decks.push(json!({
                "FaceURL": face_url,
                "BackURL": back_url,
                "NumHeight": page.height,
                "NumWidth": page.width,
            }));
        }
        decks
    };
    let linear_piles = piles
        .into_iter()
        .map(|pile: Pile| <LinearPile as TryFrom<(Pile, &[RenderedPage])>>::try_from((pile, pages)))
        .collect::<Result<Vec<_>>>()
        .context("Failed to linearize piles")?;
    let mut stacks = Vec::with_capacity(linear_piles.len());
    for (i, pile) in linear_piles.iter().enumerate() {
        let root_transform: Value = {
            let mut t = base_transform.clone();
            t["posX"] = json!(3.0 * (i as f64));
            t["rotZ"] = if pile.face_up {
                json!(0.0)
            } else {
                json!(180.0)
            };
            t
        };

        let decks_json: Value = {
            let mut page_ids = HashSet::with_capacity(decks_json_objs.len());
            for (_card, card_id) in pile.cards.iter() {
                let page_id = (*card_id as usize) / 100;
                page_ids.insert(page_id);
            }
            assert!(!page_ids.is_empty());
            let mut pages = json!({});
            for page_id in page_ids {
                pages[format!("{}", page_id)] = decks_json_objs[page_id - 1].clone();
            }
            pages
        };

        let mut stack = json!({
            "ColorDiffuse": color,
            "CustomDeck": decks_json.clone(),
            "Grid": true,
            "Locked": false,
            "Snap": true,
            "Transform": root_transform,
        });

        match pile.cards.as_slice() {
            [(card, card_id)] => {
                stack["Name"] = json!("Card");
                stack["Nickname"] = json!(card.combined_name());
                stack["CardID"] = json!(card_id);
                if let Ok(d) = card.description() {
                    stack["Description"] = json!(d);
                }
            }
            cards => {
                let card_count = cards.len();
                stack["Name"] = json!("Deck");
                stack["Nickname"] = json!(deck_title);
                stack["Description"] = json!(format!("Generated at {}", Utc::now().to_rfc2822()));
                let mut deck_ids = Vec::with_capacity(card_count);
                let mut contained_objects = Vec::with_capacity(card_count);
                for (card, deck_id) in pile.cards.iter() {
                    deck_ids.push(*deck_id);
                    let mut card_json = json!({
                        "Name": "Card",
                        "CardID": deck_id,
                        "ColorDiffuse": color,
                        "CustomDeck": decks_json.clone(),
                        "Transform": base_transform.clone(),
                        "Nickname": json!(card.names().first()),
                    });
                    if let Ok(d) = card.description() {
                        card_json["Description"] = json!(d);
                    }
                    contained_objects.push(card_json);
                }
                stack["DeckIDs"] = Value::from(deck_ids);
                stack["ContainedObjects"] = Value::from(contained_objects);
            }
        }
        stacks.push(stack);
    }

    Ok(json!({
        "ObjectStates": stacks,
    }))
}

pub async fn render_deck<R: AsyncCommands>(
    api: Arc<ScryfallApi>,
    db: &mut impl Executor<Database = Postgres>,
    redis: &mut R,
    deck: &Deck,
) -> Result<RenderedDeck> {
    let _guard = wait_for_lock(redis, deck).await?;

    info!("Rendering deck {} ({})", deck.title, deck.id);
    let piles = collect_card_piles(db, deck)
        .await
        .context("Failed to collect and sort cards")?;
    let rendered_pages = make_pages(Arc::clone(&api), redis, &deck, Arc::new(piles.clone()))
        .await
        .context("Failed to render piles to images")?;
    let saved_pages = save_pages(redis, &deck, rendered_pages)
        .await
        .context("Failed to save pages")?;
    let json = render_piles_to_json(&deck.title, piles, saved_pages.as_slice())
        .context("Failed to render piles to TTS JSON format")?;
    notify_user(
        redis,
        deck.user_id,
        s2f::Notification::RenderProgress {
            deck_id: deck.id,
            progress: s2f::RenderProgress::Rendered,
        },
    )
    .await?;

    Ok(RenderedDeck {
        json_description: json,
        rendered_at: Utc::now(),
        pages: saved_pages,
    })
}
