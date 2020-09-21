#![deny(warnings)]
#![allow(unused_braces)] // bug in rustdoc: https://github.com/rust-lang/rust/issues/70814

#[macro_use]
extern crate log;

use std::rc::Rc;
use ttsmagic_types::server_to_frontend as s2f;
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
    fatal_errors: Vec<s2f::Error>,
}

pub enum Msg {
    IgnoreError(usize),
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
    let mut host = loc.host()?;
    if host.ends_with(":8123") {
        host = host.replace(":8123", ":8124");
    }
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
        let fatal_errors = vec![];
        // // Sample errors
        //     s2f::Error {
        //         user_message: "This is the user message".to_owned(),
        //         details: Some("This is the detailed message.".to_owned()),
        //     },
        //     s2f::Error {
        //         user_message:
        //             "This is a rather long sample message without any details attached to it"
        //                 .to_owned(),
        //         details: None,
        //     },
        // ];
        Model {
            link,
            socket: Rc::new(socket),
            fatal_errors,
        }
    }

    fn update(&mut self, msg: Self::Message) -> ShouldRender {
        match msg {
            Msg::IgnoreError(i) => {
                if i < self.fatal_errors.len() {
                    self.fatal_errors.remove(i);
                    true
                } else {
                    false
                }
            }
            Msg::WS(ws_msg) => match &*ws_msg {
                s2f::ServerToFrontendMessage::FatalError(e) => {
                    let details = match &e.details {
                        Some(details) => format!("\n{}", details),
                        None => format!(""),
                    };
                    error!("Fatal error occurred! {}{}", &e.user_message, details);
                    self.fatal_errors.push((*e).clone());
                    true
                }
                _ => true,
            },
        }
    }

    fn view(&self) -> Html {
        let fatal_errors = if self.fatal_errors.is_empty() {
            html! { <></> }
        } else {
            html! {
                <div id="fatal-errors">
                    <h3> { "An error occurred!" } </h3>
                    <p> { "You may wish to reload the page and try again." } </p>
                    <hr />
                    { for self.fatal_errors.iter().enumerate().map(|(i, e)| Self::view_fatal_error(&self, i, e)) }
                </div>
            }
        };
        html! {
            <div id="content">
                <h1> {"MtG → Tabletop Simulator Deck Builder"} </h1>
                { fatal_errors }
                <deck_renderer::DeckRenderer socket=self.socket.clone() />
                <deck_list::DeckList socket=self.socket.clone() />
                <footer>
                  <a href="/logout/"> { "Sign out" } </a>
                </footer>
            </div>
        }
    }
}

impl Model {
    fn view_fatal_error(&self, i: usize, e: &s2f::Error) -> Html {
        let summary = html! {
            <div class="summary-wrapper">
            <p class="summary">
                <span> { "Error: "} { &e.user_message } </span>
                <button onclick=self.link.callback(move |_| Msg::IgnoreError(i))>
                    { "✕" }
                </button>
            </p>
            </div>
        };
        match e.details.as_ref() {
            Some(details) if details != &e.user_message => {
                html! {
                    <details class="fatal-error">
                        <summary> { summary } </summary>
                        <pre><code>{ details.as_str() }</code></pre>
                    </details>
                }
            }
            _ => html! {
                <p class="fatal-error"> { summary } </p>
            },
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
