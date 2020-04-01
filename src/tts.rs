use async_std::{fs, path::Path, prelude::*, sync::Arc, task::spawn};
use chrono::prelude::*;
use failure::{format_err, Error, ResultExt};
use image::{imageops, RgbImage};
use serde_json::{json, Value};
use smallvec::SmallVec;
use sqlx::{Executor, Postgres};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    convert::{TryFrom, TryInto},
    str::FromStr,
};

use crate::{
    deck::{Deck, DeckId},
    files::{MediaFile, StaticFiles},
    scryfall::{self, api::ScryfallApi, ScryfallCard, ScryfallId},
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

async fn get_tokens(
    api: &ScryfallApi,
    db: &mut impl Executor<Database = Postgres>,
    cards: impl Iterator<Item = ScryfallCard>,
) -> Result<Vec<ScryfallCard>, Error> {
    let (size_hint, _) = cards.size_hint();
    let mut work_queue = VecDeque::with_capacity(size_hint);
    for card in cards {
        work_queue.push_back(card);
    }
    let mut seen_ids = HashSet::with_capacity(work_queue.len());
    let mut parts: HashMap<ScryfallId, ScryfallCard> = HashMap::with_capacity(work_queue.len());

    while let Some(card) = work_queue.pop_front() {
        if !seen_ids.insert(card.id()?) {
            // We've seen this card before, don't reprocess it.
            continue;
        }
        let all_parts: &Vec<Value> = match card.raw_json().get("all_parts").map(Value::as_array) {
            None => continue,
            Some(None) => continue,
            Some(Some(all_parts)) => all_parts,
        };
        let card_name = card.combined_name().to_string();
        for (i, part_json) in all_parts.iter().enumerate() {
            let raw_part_id = part_json
                .get("id")
                .ok_or_else(|| {
                    format_err!(
                        "Related card object {} (for card {}) is missing its \"id\" field.",
                        i,
                        card_name,
                    )
                })
                .with_context(|_| format!("Getting related cards for {}", card_name))?
                .as_str()
                .ok_or_else(|| {
                    format_err!(
                        "Related card object {} (for card {}) \"id\" field is not a string.",
                        i,
                        card_name
                    )
                })
                .with_context(|_| format!("Getting related cards for {}", card_name))?;
            let part_id = ScryfallId::from_str(raw_part_id)
                .with_context(|_| format!("Getting related cards for {}", card_name))?;
            let component_type = match part_json.get("component") {
                None => {
                    debug!(
                        "Missing \"component\" field on related part for card {}",
                        card_name
                    );
                    continue;
                }
                Some(c_value) => c_value
                    .as_str()
                    .ok_or_else(|| {
                        format_err!(
                            "Related card {} (for card {}) \"component\" field is not a string.",
                            part_id,
                            card_name
                        )
                    })
                    .with_context(|_| {
                        format!("Getting related card {} for {}", part_id, card_name)
                    })?,
            };
            match component_type {
                "combo_piece" => {
                    debug!(
                        "Found combo piece {} related to card {}",
                        part_id, card_name
                    );
                    continue;
                }
                "meld_part" => {
                    debug!("Found meld part {} related to card {}", part_id, card_name);
                    continue;
                }
                "meld_result" => debug!(
                    "Found meld result {} related to card {}",
                    part_id, card_name
                ),
                "token" => debug!("Found token {} related to card {}", part_id, card_name),
                other => warn!(
                    "Found unexpected related card component type {:?} for {} related to card {}",
                    other, part_id, card_name
                ),
            };

            if seen_ids.contains(&part_id) {
                debug!("Already seen part {}", part_id);
                continue;
            }
            let part_card = scryfall::ensure_card(api, db, part_id)
                .await
                .with_context(|_| format!("Getting related card {} for {}", part_id, card_name))?;
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
    fn new_face_up(cards: Vec<(ScryfallCard, u8)>) -> Result<Self, Error> {
        if cards.is_empty() {
            Err(format_err!("Cannot make a pile of zero cards"))
        } else {
            Ok(Pile {
                cards,
                face_up: true,
            })
        }
    }

    fn new_face_down(cards: Vec<(ScryfallCard, u8)>) -> Result<Self, Error> {
        if cards.is_empty() {
            Err(format_err!("Cannot make a pile of zero cards"))
        } else {
            Ok(Pile {
                cards,
                face_up: false,
            })
        }
    }

    async fn expand_full_face_lands(
        self,
        db: &mut impl Executor<Database = Postgres>,
    ) -> Result<Self, Error> {
        let mut new_cards = Vec::with_capacity(self.cards.len() * 2);

        for (card, count) in self.cards {
            if let Some(bl_type) = card.basic_land_type() {
                debug!("Found a basic land: {}x {}", count, card.combined_name());
                let full_faced_cards = scryfall::full_faced_lands(db, bl_type).await?;
                if full_faced_cards.is_empty() {
                    warn!(
                        "Didn't find any full faced lands for basic land type {:?}",
                        bl_type
                    );
                } else {
                    debug!(
                        "Found {} full faced {:?} cards",
                        full_faced_cards.len(),
                        bl_type
                    );
                    let mut by_id: HashMap<ScryfallId, (&ScryfallCard, u8)> = HashMap::new();
                    {
                        use rand::Rng;
                        let mut rng = rand::thread_rng();
                        for _ in 0..count {
                            let i = rng.gen_range(0, full_faced_cards.len());
                            let full_faced_card = &full_faced_cards[i];
                            let full_faced_card_id = full_faced_card.id()?;
                            let entry = by_id
                                .entry(full_faced_card_id)
                                .or_insert((full_faced_card, 0));
                            entry.1 += 1;
                        }
                    }
                    let mut full_faced_count = 0;
                    for (_, (ff_card, ff_count)) in by_id {
                        debug!(
                            "Adding {}x {} from {} instead of generic {:?}",
                            ff_count,
                            ff_card.id()?,
                            ff_card
                                .raw_json()
                                .get("set_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown set"),
                            bl_type,
                        );
                        new_cards.push((ff_card.clone(), ff_count));
                        full_faced_count += ff_count;
                    }
                    assert_eq!(full_faced_count, count);
                    continue;
                }
            }

            new_cards.push((card, count));
        }

        Ok(Self {
            cards: new_cards,
            face_up: self.face_up,
        })
    }
}

#[derive(Clone, Debug)]
struct LinearPile {
    cards: Vec<(ScryfallCard, u16)>,
    face_up: bool,
}

impl TryFrom<(Pile, &'_ [RenderedPage])> for LinearPile {
    type Error = failure::Error;

    fn try_from((pile, pages): (Pile, &'_ [RenderedPage])) -> Result<LinearPile, Self::Error> {
        assert!(pile.cards.len() < 100);
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
                None => return Err(format_err!("Card {} not found in pages", card_id)),
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
    api: &ScryfallApi,
    db: &mut impl Executor<Database = Postgres>,
    deck: &Deck,
) -> Result<Piles, Error> {
    let deck_url = deck.url.clone();

    let mut main_deck = {
        let mut pile = Vec::with_capacity(deck.main_deck.len());
        for (_, (card, count)) in deck.main_deck.iter() {
            pile.push((card.clone(), count.clone()));
        }
        pile.sort_by_key(|(c, _)| c.combined_name());
        pile
    };

    fn count_cards<'a>(cards: impl Iterator<Item = &'a (ScryfallCard, u8)>) -> usize {
        cards.map(|(_, count)| *count as usize).sum()
    }

    let mut commanders_pile: Vec<(ScryfallCard, u8)> = vec![];
    if count_cards(main_deck.iter()) == 100
        && main_deck
            .iter()
            .all(|(c, _)| match c.legal_in("commander") {
                Ok(true) => true,
                Ok(false) => {
                    debug!(
                        "Card {} disqualified deck from being a commander deck",
                        c.combined_name()
                    );
                    false
                }
                Err(e) => {
                    error!(
                        "Got error when checking commander legality of main deck: {}",
                        e
                    );
                    true
                }
            })
    {
        debug!("Looks like a commander deck. Searching for the commander now...");
        // Dig out the commander and put it in its own pile.
        let (possible_commanders, non_commanders) = main_deck
            .into_iter()
            .partition::<Vec<(ScryfallCard, u8)>, _>(|(c, count)| {
                *count == 1 && c.can_be_a_commander().unwrap_or(false)
            });
        if !possible_commanders.is_empty() {
            info!(
                "Found {} possible commander{}: {:?}",
                possible_commanders.len(),
                if possible_commanders.len() == 1 {
                    ""
                } else {
                    "s"
                },
                possible_commanders
                    .iter()
                    .map(|(c, _)| format!("{}", c.combined_name()))
                    .collect::<Vec<_>>(),
            );
        }
        main_deck = non_commanders;
        commanders_pile = possible_commanders;
    }

    let main_deck_count = count_cards(main_deck.iter());
    info!(
        "Found main deck with {} card{}: {:?}",
        main_deck_count,
        if main_deck_count == 1 { "" } else { "s" },
        main_deck
            .iter()
            .map(|(c, count)| format!("{}x {}", count, c.combined_name()))
            .collect::<Vec<_>>(),
    );
    if main_deck.is_empty() {
        return Err(format_err!(
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
    let sideboard_count = count_cards(sideboard.iter());
    info!(
        "Found sideboard with {} card{}: {:?}",
        sideboard_count,
        if sideboard_count == 1 { "" } else { "s" },
        sideboard
            .iter()
            .map(|(c, count)| format!("{}x {}", count, c.combined_name()))
            .collect::<Vec<_>>(),
    );
    let tokens = get_tokens(
        api,
        db,
        main_deck
            .iter()
            .chain(sideboard.iter())
            .map(|(c, _)| c.clone()),
    )
    .await
    .with_context(|_| format!("Getting tokens for deck {}", deck_url))?;
    let tokens: Vec<_> = tokens.into_iter().map(|t| (t, 1)).collect();
    let tokens_count = count_cards(tokens.iter());
    info!(
        "Found {} token{}: {:?}",
        tokens_count,
        if tokens_count == 1 { "" } else { "s" },
        tokens
            .iter()
            .map(|(c, _)| c.combined_name())
            .collect::<Vec<_>>()
    );
    let mut piles = SmallVec::new();

    if !commanders_pile.is_empty() {
        piles.push(Pile::new_face_up(commanders_pile)?);
    }
    assert!(!main_deck.is_empty()); // checked earlier
    piles.push(
        Pile::new_face_down(main_deck)?
            .expand_full_face_lands(db)
            .await?,
    );
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
    fn new(expected_cards: usize) -> Result<Self, Error> {
        let expected_cards: u32 = expected_cards.try_into()?;
        const VALID_WIDTH_HEIGHTS: &[(u32, u32)] = &[
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 5),
            (6, 6),
            (7, 7),
            (8, 7),
            (9, 7),
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
            image: new_blank_page(width, height)?,
            card_mapping: HashMap::new(),
        };
        Ok(page)
    }
}

const CARD_WIDTH: u32 = 745;
const CARD_HEIGHT: u32 = 1040;

fn fixup_size(image: RgbImage) -> RgbImage {
    const CARD_SIZE: (u32, u32) = (CARD_WIDTH, CARD_HEIGHT);
    if image.dimensions() == CARD_SIZE {
        image
    } else {
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
    }
}

fn add_to_page(page: &mut RgbImage, card: RgbImage, row: u32, column: u32) {
    let card = fixup_size(card);
    imageops::overlay(page, &card, column * CARD_WIDTH, row * CARD_HEIGHT);
}

lazy_static::lazy_static! {
    static ref HIDDEN_IMAGE: RgbImage = {
        debug!("Loading hidden image");
        let bytes = StaticFiles::get("ttsmagic_hidden_face.png")
            .expect("ttsmagic_hidden_face.png is missing from static folder");
        let image = image::load_from_memory(&bytes).expect("Failed to load static file ttsmagic_hidden_face.png").to_rgb();
        fixup_size(image)
    };
}

fn new_blank_page(cards_wide: u32, cards_high: u32) -> Result<RgbImage, Error> {
    debug!(
        "Creating new blank page image ({} cards across and {} cards tall)",
        cards_wide, cards_high
    );
    let mut page = RgbImage::new(CARD_WIDTH * cards_wide, CARD_HEIGHT * cards_high);
    let row = cards_high - 1;
    let column = cards_wide - 1;
    add_to_page(&mut page, HIDDEN_IMAGE.clone(), row, column);
    Ok(page)
}

// macro_rules! time_block {
//     ($label:expr, $block:block) => {{
//         let start = Utc::now();
//         let value = $block;
//         let end = Utc::now();
//         let delta = end - start;
//         trace!("{} took {}", $label, delta);
//         value
//     }};
// }

// async fn load_card(
//     api: Arc<ScryfallApi>,
//     root: PathBuf,
//     seen_cards: Arc<Mutex<HashSet<ScryfallId>>>,
// ) -> impl Fn(usize, usize, &ScryfallCard) -> Result<Option<RgbImage>, Error> {
//     move |i, j, card| {}
// }

async fn make_pages<P: AsRef<Path>>(
    api: Arc<ScryfallApi>,
    root: P,
    piles: Arc<Piles>,
) -> Result<Vec<Page>, Error> {
    // TODO: this always renders pages very large, but we could make them
    // smaller for pages that don't need that many slots.
    let root = fs::canonicalize(root).await?;
    let mut page_images = Vec::with_capacity(piles.len());
    let mut current_page = Page::new(piles.iter().map(|p| p.cards.len()).sum())?;
    let mut card_load_futures: Vec<
        Box<dyn Future<Output = Result<(ScryfallCard, RgbImage), Error>> + 'static>,
    > = vec![];
    // let cards_result: Result<Vec<(usize, usize, ScryfallCard, RgbImage)>, Error> =
    for pile in piles.iter() {
        for (card, _count) in pile.cards.iter() {
            let task_card = card.clone();
            let root = root.clone();
            let api = Arc::clone(&api);
            let load_image_future = async move {
                let card_id: ScryfallId = task_card.id()?;
                let image = task_card
                    .ensure_image(&root, &api)
                    .await
                    .with_context(|_| {
                        format!(
                            "Loading image for card {} ({})",
                            task_card.combined_name(),
                            card_id,
                        )
                    })?;
                let image = fixup_size(image);
                Ok::<_, Error>(image)
            };
            let wrapper_card = card.clone();
            card_load_futures.push(Box::new(spawn(async move {
                let image = load_image_future.await?;
                Ok((wrapper_card, image))
            })) as Box<dyn Future<Output = _>>);
        }
    }
    let image_count = card_load_futures.len();
    let mut images_stream =
        crate::utils::futures_iter_to_stream(card_load_futures.into_iter()).enumerate();
    while let Some((k, card_info)) = images_stream.next().await {
        let (card, image) = card_info?;
        let card_id = card.id()?;
        if (current_page.card_mapping.len() as u32)
            >= ((current_page.width * current_page.height) - 1)
        {
            debug!(
                "Finalizing page {} and starting another.",
                page_images.len()
            );
            page_images.push(current_page);
            current_page = Page::new(image_count - k - 1)?;
        }
        let page_index: u32 = current_page.card_mapping.len() as u32;
        let cards_per_page = (current_page.width * current_page.height) - 1;
        let row = (page_index % cards_per_page) / current_page.width;
        let column = (page_index % cards_per_page) % current_page.width;
        assert!(row < current_page.height);
        assert!(column < current_page.width);
        add_to_page(&mut current_page.image, image, row, column);
        current_page
            .card_mapping
            .insert(card_id, page_index.try_into().unwrap());
    }
    if !current_page.card_mapping.is_empty() {
        page_images.push(current_page);
    }

    Ok(page_images)
}

async fn save_pages<P: AsRef<Path>>(
    root: P,
    deck_id: DeckId,
    pages: Vec<Page>,
) -> Result<Vec<RenderedPage>, Error> {
    let mut saved_pages = Vec::with_capacity(pages.len());
    for (i, page) in pages.into_iter().enumerate() {
        let deck_uuid = format!("{}", deck_id.as_uuid());
        let page_filename = format!(
            "pages/{}/{}/{}_{}.jpg",
            &deck_uuid[0..2],
            &deck_uuid[2..4],
            deck_uuid,
            i,
        );
        let f = MediaFile::create(&root, &page_filename)?;
        page.image.save(&f.path())?;
        let saved = f.finalize().await?;
        info!("Saved page image {}", saved.path().to_string_lossy());
        saved_pages.push(RenderedPage {
            width: page.width,
            height: page.height,
            image: saved,
            card_mapping: page.card_mapping,
        });
    }

    Ok(saved_pages)
}

fn render_piles_to_json<'a>(
    deck_title: &str,
    piles: Piles,
    pages: &'a [RenderedPage],
) -> Result<Value, Error> {
    // let mut pages = Vec::with_capacity(saved_pages.len());
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
    let decks_json = {
        let back_url = StaticFiles::get_url("backing.jpg")?.to_string();
        let mut decks = json!({});
        for (i, page) in pages.iter().enumerate() {
            let face_url = page.image.url()?.to_string();
            decks[(i + 1).to_string()] = json!({
                "FaceURL": face_url,
                "BackURL": back_url,
                "NumHeight": page.height,
                "NumWidth": page.width,
            });
        }
        decks
    };
    let linear_piles = piles
        .into_iter()
        .map(|pile: Pile| <LinearPile as TryFrom<(Pile, &[RenderedPage])>>::try_from((pile, pages)))
        .collect::<Result<Vec<_>, Error>>()?;
    let mut stacks = Vec::with_capacity(linear_piles.len());
    for (i, pile) in linear_piles.iter().enumerate() {
        let root_transform = {
            let mut t = base_transform.clone();
            t["posX"] = json!(3.0 * (i as f64));
            t["rotZ"] = if pile.face_up {
                json!(0.0)
            } else {
                json!(180.0)
            };
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
            [(card, deck_id)] => {
                stack["Name"] = json!("Card");
                stack["Nickname"] = json!(card.combined_name());
                stack["CardID"] = json!(deck_id);
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
                    contained_objects.push(json!({
                        "Name": "Card",
                        "CardID": deck_id,
                        "ColorDiffuse": color,
                        "CustomDeck": decks_json.clone(),
                        "Transform": base_transform.clone(),
                        "Nickname": json!(card.names().first()),
                    }));
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

pub async fn render_deck<P: AsRef<Path>>(
    api: Arc<ScryfallApi>,
    db: &mut impl Executor<Database = Postgres>,
    root: P,
    deck: &Deck,
) -> Result<RenderedDeck, Error> {
    let piles = collect_card_piles(&api, db, deck)
        .await
        .context("Collecting and sorting cards")?;
    let rendered_pages = make_pages(Arc::clone(&api), &root, Arc::new(piles.clone()))
        .await
        .context("Rendering piles to images")?;
    let saved_pages = save_pages(&root, deck.id, rendered_pages).await?;
    let json = render_piles_to_json(&deck.title, piles, saved_pages.as_slice())?;

    Ok(RenderedDeck {
        json_description: json,
        rendered_at: Utc::now(),
        pages: saved_pages,
    })
}
