#![deny(warnings)]
#![allow(unused_braces)] // bug in rustdoc: https://github.com/rust-lang/rust/issues/70814

#[macro_use]
extern crate log;

use std::rc::Rc;
use wasm_bindgen::prelude::*;
use yew::prelude::*;

mod deck_list;
mod deck_renderer;
mod remote_resource;
mod ws;

pub struct Model {
    #[allow(unused)]
    link: ComponentLink<Self>,
    socket: Rc<ws::WebSocket>,
}

pub enum Msg {
    WS(ws::Message),
}

fn make_ws_url() -> Result<url::Url, JsValue> {
    let window =
        web_sys::window().ok_or_else(|| JsValue::from_str("No window property available"))?;
    let loc = window.location();
    let web_proto = loc.protocol()?;
    let ws_proto = match web_proto.as_str() {
        "http:" => "ws:",
        "https:" => "wss:",
        other => {
            return Err(JsValue::from_str(&format!(
                "Unexpected page protocol {:?}, expected \"http:\" or \"https:\"",
                other
            )))
        }
    };
    let host = loc.host()?;
    let url_string = format!("{}//{}/ws/", ws_proto, host);
    url::Url::parse(&url_string)
        .map_err(|e| JsValue::from_str(&format!("Error parsing WebSocket URL: {}", e)))
}

impl Component for Model {
    type Message = Msg;
    type Properties = ();

    fn create(_: Self::Properties, link: ComponentLink<Self>) -> Self {
        let ws_url = make_ws_url().unwrap();
        let socket = ws::WebSocket::new(ws_url).unwrap();
        socket.register_message_callback(link.callback(Msg::WS));
        Model {
            link,
            socket: Rc::new(socket),
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            _ => true,
        }
    }

    fn view(&self) -> Html {
        html! {
            <div id="content">
                <h1> {"MtG → Tabletop Simulator Deck Builder"} </h1>
                <deck_renderer::DeckRenderer socket=self.socket.clone() />
                <deck_list::DeckList socket=self.socket.clone() />
                <footer>
                  <a href="/logout/"> { "Sign out" } </a>
                </footer>
            </div>
        }
    }
}

#[wasm_bindgen]
pub fn run_app() -> Result<(), JsValue> {
    console_log::init_with_level(log::Level::Debug)
        .map_err(|e| JsValue::from_str(&format!("Failed to set log level: {}", e)))?;

    yew::start_app::<Model>();

    Ok(())
}
