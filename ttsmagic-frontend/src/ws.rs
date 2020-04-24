// use js_sys::Function;
use anyhow::format_err;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use ttsmagic_types::{frontend_to_server as f2s, server_to_frontend as s2f};
use url::Url;
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{ErrorEvent, MessageEvent, WebSocket as Native};
use yew::callback::Callback;

pub type Message = Rc<s2f::ServerToFrontendMessage>;

struct Inner {
    message_callbacks: Vec<Callback<Message>>,
    socket: Option<Native>,
    pending: VecDeque<String>,
    url: Url,
    // protocol: String,
}

pub struct WebSocket {
    inner: Rc<RefCell<Inner>>,
}

// pub const PROTO: &'static str = "ttsmagic-ws-protocol-v2";

impl WebSocket {
    pub fn new(url: Url) -> Result<Self, JsValue> {
        let inner = Inner {
            message_callbacks: vec![],
            socket: None,
            pending: VecDeque::new(),
            url,
            // protocol: protocol.to_string(),
        };
        let inner = Rc::new(RefCell::new(inner));
        Self::connect(inner.clone())?;

        Ok(Self { inner })
    }

    fn connect(inner_cell: Rc<RefCell<Inner>>) -> Result<(), JsValue> {
        let mut inner = inner_cell.borrow_mut();
        let url_str = format!("{}", inner.url);
        debug!("Connecting websocket to {}...", url_str);
        let socket = Native::new(&url_str)?;
        debug!("Successfully connected websocket");

        let onerror_fn = Closure::wrap(Box::new(move |e: ErrorEvent| {
            error!("Got websocket error: {:?}", e);
        }) as Box<dyn FnMut(ErrorEvent)>);
        socket.set_onerror(Some(onerror_fn.as_ref().unchecked_ref()));
        onerror_fn.forget();

        let onopen_inner_cell = inner_cell.clone();
        let onopen_fn = Closure::once(Box::new(move || {
            let mut inner = onopen_inner_cell.borrow_mut();
            while let Some(msg_str) = inner.pending.pop_front() {
                let socket = inner
                    .socket
                    .as_ref()
                    .expect("WebSocket.inner.socket was unexpectedly None!");
                socket.send_with_str(&msg_str).unwrap();
            }
        }) as Box<dyn FnOnce()>);
        socket.set_onopen(Some(onopen_fn.as_ref().unchecked_ref()));
        onopen_fn.forget();

        let onclose_inner_cell = inner_cell.clone();
        let onclose_fn = Closure::once(Box::new(move || {
            debug!("Got websocket close event");
            if let Err(e) = Self::connect(onclose_inner_cell) {
                error!("Failed to reconnect websocket: {:?}", e);
            }
        }) as Box<dyn FnOnce()>);
        socket.set_onclose(Some(onclose_fn.as_ref().unchecked_ref()));
        onclose_fn.forget();

        let onmessage_inner_cell = inner_cell.clone();
        let onmessage_inner_fn = move |event: MessageEvent| -> anyhow::Result<Message> {
            let message_data = event.data();
            let message_string = message_data.as_string().ok_or_else(|| {
                format_err!("Got non-string websocket message {:?}", message_data)
            })?;
            let parsed = serde_json::from_str(&message_string)?;
            Ok(Rc::new(parsed))
        };
        let onmessage_fn = Closure::wrap(Box::new(move |event: MessageEvent| {
            debug!("Got websocket message event: {:?}", event);
            let result = match onmessage_inner_fn(event) {
                Ok(r) => r,
                Err(e) => {
                    error!("Error getting message from server: {}", e);
                    return;
                }
            };
            let inner = onmessage_inner_cell.borrow();
            for cb in inner.message_callbacks.iter() {
                cb.emit(result.clone())
            }
        }) as Box<dyn Fn(MessageEvent)>);
        socket.set_onmessage(Some(onmessage_fn.as_ref().unchecked_ref()));
        onmessage_fn.forget();
        inner.socket = Some(socket);

        Ok(())
    }

    pub fn register_message_callback(&self, cb: Callback<Message>) {
        let mut inner = self.inner.borrow_mut();
        inner.message_callbacks.push(cb);
    }

    pub fn send(&self, message: f2s::FrontendToServerMessage) -> Result<(), JsValue> {
        let encoded = serde_json::to_string(&message).map_err(|e| {
            let err_msg = format!("Failed to parse incoming websocket message: {}", e);
            JsValue::from_str(&err_msg)
        })?;
        let mut inner = self.inner.borrow_mut();
        let socket = inner
            .socket
            .as_ref()
            .expect("WebSocket.inner.socket was unexpectedly None!");
        if socket.ready_state() == 1 {
            socket.send_with_str(&encoded)
        } else {
            inner.pending.push_back(encoded);
            Ok(())
        }
    }
}
