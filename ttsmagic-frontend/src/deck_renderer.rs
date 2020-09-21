use std::rc::Rc;
use ttsmagic_types::frontend_to_server::FrontendToServerMessage as F2SMsg;
use url::Url;
use yew::prelude::*;

pub enum Msg {
    RenderDeck,
    SetRawUrl(String),
}

#[derive(Clone, Properties)]
pub struct Props {
    pub socket: Rc<crate::ws::WebSocket>,
}

pub struct DeckRenderer {
    #[allow(unused)]
    link: ComponentLink<Self>,
    raw_url: String,
    parsed_url: Result<Url, String>,
    socket: Rc<crate::ws::WebSocket>,
}

impl Component for DeckRenderer {
    type Message = Msg;
    type Properties = Props;

    fn create(props: Self::Properties, link: ComponentLink<Self>) -> Self {
        Self {
            link,
            raw_url: String::new(),
            parsed_url: Err(format!("Please enter a URL")),
            socket: props.socket,
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::RenderDeck => match self.parsed_url.clone() {
                Ok(url) => {
                    let msg = F2SMsg::RenderDeck { url: url.clone() };
                    self.socket.send(msg).unwrap();
                    self.parsed_url = Err(format!("Please enter a URL"));
                    true
                }
                Err(_) => false,
            },
            Msg::SetRawUrl(s) => {
                self.parsed_url = Url::parse(&s).map_err(|e| format!("Invalid URL: {}", e));
                self.raw_url = s;
                true
            }
        }
    }

    fn view(&self) -> Html {
        html! {
            <div id="create-deck-form">
                <label for="create-url"> { "URL:" } </label>
                <input id="create-url"
                    autofocus=true
                    type="text"
                    placeholder="https://deckbox.org/sets/XXXXXX"
                    value=&self.raw_url
                    oninput=self.link.callback(|e: InputData| Msg::SetRawUrl(e.value))
                />
                <button value="Convert!"
                    disabled=self.parsed_url.is_err()
                    onclick=self.link.callback(|_| Msg::RenderDeck)
                >
                    { "Convert!" }
                </button>
            </div>
        }
    }
}
