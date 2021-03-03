use std::{num::NonZeroU16, rc::Rc};
use ttsmagic_types::{
    frontend_to_server::FrontendToServerMessage as F2SMsg, server_to_frontend as s2f,
    server_to_frontend::ServerToFrontendMessage as S2FMsg, Deck, DeckId,
};
use yew::prelude::*;

use crate::remote_resource::RemoteResource;

pub enum DeckStatus {
    Loading,
    Waiting { queue_length: NonZeroU16 },
    RenderingCards { complete: u16, total: NonZeroU16 },
    RenderingPages { complete: u16, total: NonZeroU16 },
    Complete,
    // Error(Option<String>),
}

struct DeckInfo {
    deck: Deck,
    status: DeckStatus,
}

impl DeckInfo {
    fn bg_gradient_css(&self) -> String {
        const BG_ALPHA: f32 = 0.2;
        const BG: (u8, u8, u8) = (0xcc, 0xcc, 0xcc);
        // # Bold colors
        // const WHITE: (u8, u8, u8) = (249, 250, 244);
        const BLUE: (u8, u8, u8) = (14, 104, 171);
        const BLACK: (u8, u8, u8) = (21, 11, 0);
        const RED: (u8, u8, u8) = (211, 32, 42);
        const GREEN: (u8, u8, u8) = (0, 115, 62);
        // # Pale colors
        const WHITE: (u8, u8, u8) = (248, 231, 185);
        // const BLUE: (u8, u8, u8) = (179, 206, 234);
        // const BLACK: (u8, u8, u8) = (166, 159, 157);
        // const RED: (u8, u8, u8) = (235, 159, 130);
        // const GREEN: (u8, u8, u8) = (196, 211, 202);

        let mut css = "background: #eee; background: linear-gradient(120deg".to_owned();
        let mut current: f32 = 0.0;
        fn append_section(color: (u8, u8, u8), width: f32, current: &mut f32, css: &mut String) {
            use std::fmt::Write;
            let start = if *current > 0.0 {
                *current + (width / 4.0)
            } else {
                *current
            };
            *current += width;
            let end = if *current >= 100.0 {
                100.0
            } else {
                *current - (width / 4.0)
            };
            write!(
                css,
                ", rgba({r},{g},{b},{a:.2}) {start:.0}% {end:.0}%",
                a = BG_ALPHA,
                r = color.0,
                g = color.1,
                b = color.2,
                start = start.min(100.0),
                end = end.min(100.0),
            )
            .unwrap();
        }
        macro_rules! append_section {
            ($color:expr, $width:expr) => {
                append_section($color, $width, &mut current, &mut css);
            };
        }

        let ci = self.deck.color_identity;

        let mut sections = 0;
        if ci.white {
            sections += 1;
        }
        if ci.blue {
            sections += 1;
        }
        if ci.black {
            sections += 1;
        }
        if ci.red {
            sections += 1;
        }
        if ci.green {
            sections += 1;
        }

        if sections == 0 {
            append_section!(BG, 100.0);
        } else {
            macro_rules! append_color {
                ($name:ident => $color:expr) => {
                    if ci.$name {
                        append_section!($color, 100.0 / (sections as f32));
                    }
                };
            }
            append_color!(white => WHITE);
            append_color!(blue => BLUE);
            append_color!(red => RED);
            append_color!(black => BLACK);
            append_color!(green => GREEN);
        }
        css.push_str(");");
        css
    }
}

pub enum Msg {
    DeleteDeck(DeckId),
    FromServer(Rc<S2FMsg>),
    RebuildDeck(DeckId),
}

#[derive(Clone, Properties)]
pub struct Props {
    pub socket: Rc<crate::ws::WebSocket>,
}

pub struct DeckList {
    #[allow(unused)]
    link: ComponentLink<Self>,
    decks: RemoteResource<Vec<DeckInfo>>,
    socket: Rc<crate::ws::WebSocket>,
}

impl Component for DeckList {
    type Message = Msg;
    type Properties = Props;

    fn create(props: Self::Properties, link: ComponentLink<Self>) -> Self {
        let decks = RemoteResource::Loading;
        props
            .socket
            .register_message_callback(link.callback(Msg::FromServer));
        props.socket.send(F2SMsg::GetDecks).unwrap();
        let socket = props.socket;
        Self {
            link,
            decks,
            socket,
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        let should_render = match msg {
            Msg::DeleteDeck(deck_id) => {
                self.socket
                    .send(F2SMsg::DeleteDeck { id: deck_id })
                    .unwrap();
                true
            }
            Msg::FromServer(msg_rc) => match &*msg_rc {
                S2FMsg::DeckList { decks } => {
                    let mut deck_infos = Vec::with_capacity(decks.len());
                    for deck in decks.into_iter().cloned() {
                        let status = DeckStatus::Complete;
                        deck_infos.push(DeckInfo { deck, status });
                    }
                    self.decks = RemoteResource::Loaded(deck_infos);
                    true
                }
                S2FMsg::FatalError(s2f::Error { .. }) => false,
                S2FMsg::Notification(s2f::Notification::DeckDeleted { deck_id }) => {
                    match &mut self.decks {
                        RemoteResource::Loaded(ref mut decks) => {
                            let mut old_decks: Vec<DeckInfo> = vec![];
                            std::mem::swap(decks, &mut old_decks);
                            *decks = old_decks
                                .into_iter()
                                .filter(|di| di.deck.id != *deck_id)
                                .collect();
                            true
                        }
                        _ => false,
                    }
                }
                S2FMsg::Notification(s2f::Notification::DeckParseStarted {
                    deck_id,
                    title,
                    url,
                }) => match &mut self.decks {
                    RemoteResource::Loaded(ref mut decks) => {
                        let mut new_deck_info = Some(DeckInfo {
                            status: DeckStatus::Loading,
                            deck: Deck {
                                id: *deck_id,
                                title: title.clone(),
                                url: url.clone(),
                                rendered: false,
                                color_identity: Default::default(),
                            },
                        });
                        for di in decks.iter_mut() {
                            if &di.deck.id == deck_id {
                                if let Some(new_di) = new_deck_info.take() {
                                    *di = new_di;
                                }
                            }
                        }
                        if let Some(new_di) = new_deck_info {
                            decks.push(new_di);
                        }
                        true
                    }
                    _ => false,
                },
                S2FMsg::Notification(s2f::Notification::DeckParsed {
                    deck_id,
                    title,
                    url,
                    color_identity,
                }) => match &mut self.decks {
                    RemoteResource::Loaded(ref mut decks) => {
                        for di in decks.iter_mut() {
                            if &di.deck.id == deck_id {
                                di.status = DeckStatus::Loading;
                                di.deck.title = title.clone();
                                di.deck.url = url.clone();
                                di.deck.color_identity = color_identity.clone();
                            }
                        }
                        true
                    }
                    _ => false,
                },
                S2FMsg::Notification(s2f::Notification::Error(_)) => false,
                S2FMsg::Notification(s2f::Notification::RenderProgress { deck_id, progress }) => {
                    match &mut self.decks {
                        RemoteResource::Loaded(ref mut decks) => {
                            let status = match progress {
                                s2f::RenderProgress::Waiting { queue_length } => {
                                    DeckStatus::Waiting {
                                        queue_length: *queue_length,
                                    }
                                }
                                s2f::RenderProgress::RenderingImages {
                                    rendered_cards,
                                    total_cards,
                                } => DeckStatus::RenderingCards {
                                    complete: *rendered_cards,
                                    total: *total_cards,
                                },
                                s2f::RenderProgress::SavingPages {
                                    saved_pages,
                                    total_pages,
                                } => DeckStatus::RenderingPages {
                                    complete: *saved_pages,
                                    total: *total_pages,
                                },
                                s2f::RenderProgress::Rendered => DeckStatus::Complete,
                            };
                            let rendered = match progress {
                                s2f::RenderProgress::Rendered => true,
                                _ => false,
                            };
                            for di in decks.iter_mut() {
                                if &di.deck.id == deck_id {
                                    di.status = status;
                                    di.deck.rendered = rendered;
                                    break;
                                }
                            }
                            true
                        }
                        _ => false,
                    }
                }
            },
            Msg::RebuildDeck(deck_id) => {
                let mut url = None;
                self.decks.as_ref().map(|dis| {
                    if let Some(di) = dis.into_iter().filter(|di| di.deck.id == deck_id).next() {
                        url = Some(di.deck.url.clone());
                    }
                });
                if let Some(url) = url {
                    let msg = F2SMsg::RenderDeck { url: url };
                    self.socket.send(msg).unwrap();
                }
                false
            }
        };
        self.decks
            .as_mut()
            .map(|dis| dis.sort_by_key(|di| di.deck.title.to_lowercase()));
        should_render
    }

    fn view(&self) -> Html {
        let deck_list = match &self.decks {
            RemoteResource::Loading => html! { <p> { "Loading…" } </p> },
            RemoteResource::Loaded(decks) => {
                html! { for decks.iter().enumerate().map(|(i, di)| Self::view_deck(&self, di)) }
            }
            RemoteResource::Error(e) => {
                html! { <p> { format!("Error loading decks: {:?}", e) } </p> }
            }
        };
        html! {
            <div id="generated-decks">
                <h3> { "Your decks:" } </h3>
                <ul> { deck_list } </ul>
            </div>
        }
    }
}

impl DeckList {
    fn view_deck(&self, di: &DeckInfo) -> Html {
        let deck_id = di.deck.id;
        let download_link = if di.deck.rendered {
            html! {
                <a href={ format!("/decks/{}.json", deck_id) }, target="_blank">
                    { di.deck.title.clone() }
                </a>
            }
        } else {
            html! { { di.deck.title.clone() } }
        };
        let deck_name = html! {
            <>
                <a href=di.deck.url.to_string() target="_blank"> { "\u{1F5C3}" } </a>
                { " " }
                { download_link }
            </>
        };
        let (status_msg, progress_bar) = match &di.status {
            DeckStatus::Waiting { queue_length } => (
                {
                    let ql = queue_length.get() - 1;
                    let s = if ql == 1 { "" } else { "s" };
                    format!("Waiting… ({} deck{} ahead of this one)", ql, s)
                },
                html! { <> </> },
            ),
            DeckStatus::Loading => ("Loading…".to_string(), html! { <> </> }),
            DeckStatus::RenderingCards { complete, total } => (
                format!("Loading cards"),
                html! { <progress value={ complete } max={ total } /> },
            ),
            DeckStatus::RenderingPages { complete, total } => (
                format!(
                    "Saving page image{}",
                    if total.get() == 1 { "" } else { "" }
                ),
                html! { <progress value={ complete } max={ total } /> },
            ),
            DeckStatus::Complete => (String::new(), html! { <> </> }),
            // DeckStatus::Error(Some(e)) => format!("Error rendering deck: {}", e),
            // DeckStatus::Error(None) => "Unknown error rendering deck".to_string(),
        };
        html! {
            <li style={ di.bg_gradient_css() }>
                <span class="deck-name"> { deck_name } </span>
                <span class="deck-status"> { status_msg } { progress_bar } </span>
                <button style="flex: 0 0 auto" onclick=self.link.callback(move |_| Msg::RebuildDeck(deck_id))>
                  { "Rebuild" }
                </button>
                <button style="color: red; flex: 0 0 auto" onclick=self.link.callback(move |_| Msg::DeleteDeck(deck_id))>
                  { "✕" }
                </button>
            </li>
        }
    }
}
