use std::{num::NonZeroU16, rc::Rc};
use ttsmagic_types::{
    frontend_to_server::FrontendToServerMessage as F2SMsg, server_to_frontend as s2f,
    server_to_frontend::ServerToFrontendMessage as S2FMsg, Deck, DeckId,
};
use yew::prelude::*;

use crate::remote_resource::RemoteResource;

pub enum DeckStatus {
    Loading,
    RenderingCards { complete: u16, total: NonZeroU16 },
    RenderingPages { complete: u16, total: NonZeroU16 },
    Complete,
    // Error(Option<String>),
}

struct DeckInfo {
    deck: Deck,
    status: DeckStatus,
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
                }) => match &mut self.decks {
                    RemoteResource::Loaded(ref mut decks) => {
                        for di in decks.iter_mut() {
                            if &di.deck.id == deck_id {
                                di.status = DeckStatus::Loading;
                                di.deck.title = title.clone();
                                di.deck.url = url.clone();
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
                html! { for decks.iter().map(|di| Self::view_deck(&self, di)) }
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
                { download_link }
                { " " }
                <a href=di.deck.url.to_string() target="_blank"> { "\u{1F5C3}" } </a>
            </>
        };
        let status_msg = match &di.status {
            DeckStatus::Loading => "Loading…".to_string(),
            DeckStatus::RenderingCards { complete, total } => {
                format!("Rendered {} of {} cards", complete, total)
            }
            DeckStatus::RenderingPages { complete, total } => {
                format!("Saved {} of {} pages", complete, total)
            }
            DeckStatus::Complete => String::new(),
            // DeckStatus::Error(Some(e)) => format!("Error rendering deck: {}", e),
            // DeckStatus::Error(None) => "Unknown error rendering deck".to_string(),
        };
        html! {
            <li>
                <span class="deck-name"> { deck_name } </span>
                <span class="deck-status"> { status_msg } </span>
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
