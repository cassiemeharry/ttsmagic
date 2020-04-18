use async_std::{
    net::{IpAddr, TcpListener, TcpStream},
    prelude::*,
    task::{block_on, spawn},
};
// use async_std_tokio_compat::*;
use anyhow::{anyhow, Context, Error, Result};
use async_tungstenite::{accept_hdr_async, WebSocketStream};
use futures::sink::{Sink, SinkExt as _};
use http_0_2::status::StatusCode;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use tungstenite::Message;

use crate::{
    deck::{Deck, DeckId, DeckSummary},
    notify,
    user::User,
    utils::AsyncStdStreamWrapper,
    web::{session::SessionGetExt as _, AppState},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "tag")]
enum WsIncomingMsg {
    DeleteDeck { id: DeckId },
    GetDecks,
    GetDeckStatus { id: DeckId },
    RenderDeck { url: String },
}

#[derive(Debug, Serialize)]
struct OutboundDeck<'a> {
    id: DeckId,
    title: Cow<'a, str>,
    url: Cow<'a, str>,
}

impl<'a> From<&'a Deck> for OutboundDeck<'a> {
    fn from(deck: &'a Deck) -> OutboundDeck<'a> {
        OutboundDeck {
            id: deck.id,
            title: Cow::Borrowed(&deck.title),
            url: Cow::Borrowed(&deck.url),
        }
    }
}

impl<'a> From<&'a DeckSummary> for OutboundDeck<'a> {
    fn from(deck: &'a DeckSummary) -> OutboundDeck<'a> {
        OutboundDeck {
            id: deck.id,
            title: Cow::Borrowed(&deck.title),
            url: Cow::Borrowed(&deck.url),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case", tag = "tag")]
enum WsOutgoingMsg<'a> {
    Notification {
        #[serde(rename = "notification")]
        data: serde_json::Value,
    },
    DeckDeleted {
        id: DeckId,
    },
    DeckList {
        decks: Vec<OutboundDeck<'a>>,
    },
    DeckStatus {
        id: DeckId,
        deck: Option<OutboundDeck<'a>>,
    },
}

impl<'a> WsOutgoingMsg<'a> {
    async fn send<S: Sink<Message> + Unpin>(self, stream: &mut S) -> Result<()>
    where
        <S as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
    {
        let encoded = serde_json::to_string(&self)?;
        let msg = Message::Text(encoded);
        let () = stream.send(msg).await?;
        Ok(())
    }
}

async fn handle_connection(
    ws_stream: WebSocketStream<TcpStream>,
    state: AppState,
    user: User,
) -> Result<()> {
    debug!("Got a websocket connection for {}", user);

    let mut pubsub_conn = state
        .redis
        .get_async_connection()
        .await
        .context("Getting Redis connection for websocket")?
        .into_pubsub();
    let pubsub_stream = notify::subscribe_user(&mut pubsub_conn, user.id).await?;
    let mut pubsub_stream = AsyncStdStreamWrapper::new(pubsub_stream);
    let mut ping_timer_stream = AsyncStdStreamWrapper::new(async_std::stream::interval(
        std::time::Duration::from_secs(10),
    ));

    let mut ws_stream = AsyncStdStreamWrapper::new(ws_stream);

    loop {
        futures::select! {
            _ = ping_timer_stream.next() => {
                let now = chrono::Utc::now();
                let encoded = serde_json::to_vec(&now)?;
                ws_stream.send(Message::Ping(encoded)).await?;
            },
            redis_msg_opt = pubsub_stream.next() => {
                let redis_msg = match redis_msg_opt {
                    Some(m) => m,
                    None => {
                        warn!("Looks like Redis disconnected");
                        continue;
                    },
                };
                debug!("Got a message from Redis pubsub: {:?}", redis_msg);
                let msg = WsOutgoingMsg::Notification { data: redis_msg };
                msg.send(&mut ws_stream).await?;
            },
            ws_msg = ws_stream.next() => {
                let ws_msg = match ws_msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        error!("Error parsing WebSocket connection: {}", e);
                        break;
                    },
                    None => {
                        debug!("Looks like the websocket connection closed");
                        break;
                    },
                };
                let parsed_message = match ws_msg {
                    Message::Text(string) => serde_json::from_str(string.as_str())?,
                    Message::Binary(vec) => serde_json::from_slice(vec.as_slice())?,
                    Message::Ping(contents) => {
                        ws_stream.send(Message::Pong(contents)).await?;
                        continue;
                    }
                    Message::Pong(contents) => {
                        // match serde_json::from_slice::<chrono::DateTime<chrono::Utc>>(contents.as_slice()) {
                        //     Ok(then) => {
                        //         let now = chrono::Utc::now();
                        //         let delta = now - then;
                        //         let delta_desc = if delta.num_seconds().abs() > 1 {
                        //             format!("{}", delta)
                        //         } else {
                        //             let micros = delta.num_microseconds().unwrap();
                        //             format!("{:.1}ms", (micros as f64) / 1000.0)
                        //         };
                        //         debug!("Got a pong, took {}", delta_desc);
                        //     }
                        //     Err(e) => {
                        //         error!("Got a pong, failed to parse as datetime: {}", e);
                        //     }
                        // };
                        continue;
                    }
                    Message::Close(close_msg) => {
                        debug!("Got close message: {:?}", close_msg);
                        break;
                    }
                };
                match parsed_message {
                    WsIncomingMsg::DeleteDeck { id } => {
                        let mut db_conn = state.db_pool.acquire().await?;
                        if let Some(deck) = Deck::get_by_id(&mut db_conn, id).await? {
                            if deck.user_id != user.id {
                                Err(anyhow!("Invalid deck ID (doesn't belong to you)"))?;
                            }
                            deck.delete(&mut db_conn).await?;
                        }
                        let msg = WsOutgoingMsg::DeckDeleted { id };
                        msg.send(&mut ws_stream).await?;
                    }
                    WsIncomingMsg::GetDecks => {
                        let mut db_conn = state.db_pool.acquire().await?;
                        let full_decks = DeckSummary::get_for_user(&mut db_conn, user.id).await?;
                        let decks = {
                            let mut decks = Vec::with_capacity(full_decks.len());
                            for d in full_decks.iter() {
                                decks.push(d.into());
                            }
                            decks
                        };
                        let msg = WsOutgoingMsg::DeckList { decks };
                        msg.send(&mut ws_stream).await?;
                    }
                    WsIncomingMsg::GetDeckStatus { id } => {
                        let mut db_conn = state.db_pool.acquire().await?;
                        let full_deck_opt: Option<Deck> = Deck::get_by_id(&mut db_conn, id)
                            .await?
                            .and_then(|d| if d.user_id == user.id { Some(d) } else { None });
                        let deck_opt = full_deck_opt.as_ref().map(|d| d.into());
                        let msg = WsOutgoingMsg::DeckStatus { id, deck: deck_opt };
                        msg.send(&mut ws_stream).await?;
                    }
                    WsIncomingMsg::RenderDeck { url } => {
                        let state = state.clone();
                        let user = user.clone();
                        let user_id = user.id;
                        async_std::task::spawn_blocking::<_, ()>(move || block_on(async move {
                            let state2 = state.clone();
                            let failable = async move {
                                let mut db_conn = state.db_pool.acquire().await?;
                                let mut redis_conn = state
                                    .redis
                                    .get_async_connection()
                                    .await
                                    .context("While getting Redis to render deck at request of websocket")?;
                                notify::notify_user(
                                    &mut redis_conn,
                                    user.id,
                                    "deck_rendering",
                                    serde_json::json!({
                                        "tag": "loading",
                                        "url": url,
                                    }),
                                ).await?;
                                let mut deck = crate::deck::load_deck(&mut db_conn, &user, &url)
                                    .await
                                    .context("Loading deck at request of websocket")?;
                                notify::notify_user(
                                    &mut redis_conn,
                                    user.id,
                                    "deck_rendering",
                                    serde_json::json!({
                                        "tag": "new-deck",
                                        "id": deck.id,
                                        "title": deck.title,
                                        "url": deck.url,
                                        "json": serde_json::Value::Null,
                                    }),
                                ).await?;
                                let rendered = deck.render(state.scryfall_api.clone(), &mut db_conn, &mut redis_conn, &state.root)
                                    .await?;
                                Ok(())
                            };
                            match failable.await {
                                Ok(()) => (),
                                Err(e) => {
                                    let mut redis_conn = match state2
                                        .redis
                                        .get_async_connection()
                                        .await {
                                            Ok(conn) => conn,
                                            Err(e) => {
                                                error!("Error getting Redis connection to notify user of deck render failure");
                                                return;
                                            },
                                        };
                                    let e: Error = e;
                                    error!("Got errror when rendering deck: {:?}", e);
                                    let notify_result = notify::notify_user(
                                        &mut redis_conn,
                                        user_id,
                                        "deck_rendering",
                                        serde_json::json!({
                                            "tag": "error",
                                            "message": e.to_string(),
                                        }),
                                    ).await;
                                    match notify_result {
                                        Ok(()) => (),
                                        Err(e2) => error!("Failed to notify user of error {} because of {}", e, e2),
                                    }
                                }
                            }
                        }));
                    }
                }
            },
        }
    }
    Ok(())
}

struct ServerCallback {
    state: AppState,
    user: Option<User>,
}

impl<'a> tungstenite::handshake::server::Callback for &'a mut ServerCallback {
    fn on_request(
        self,
        request: &http_0_2::Request<()>,
        response: http_0_2::Response<()>,
    ) -> std::result::Result<http_0_2::Response<()>, http_0_2::Response<Option<String>>> {
        let err_response: std::result::Result<_, http_0_2::Response<Option<String>>> = {
            let mut resp = http_0_2::Response::new(None);
            *resp.status_mut() = StatusCode::FORBIDDEN;
            Err(resp)
        };

        // I wish this was an async function so this didn't block the main thread...
        let mut db_conn = match block_on(self.state.db_pool.acquire()) {
            Ok(c) => c,
            Err(e) => {
                error!("Error getting DB connection in WebSocket callback: {}", e);
                return err_response;
            }
        };
        let mut redis_conn = match block_on(self.state.redis.get_async_connection()) {
            Ok(c) => c,
            Err(e) => {
                error!(
                    "Error getting Redis connection in WebSocket callback: {}",
                    e
                );
                return err_response;
            }
        };
        let session_get_tuple = (&mut db_conn, &mut redis_conn, request);
        let user = match block_on(session_get_tuple.get_session()).and_then(|s| s.user) {
            Some(u) => u,
            None => return err_response,
        };

        self.user = Some(user);
        Ok(response)
    }
}

pub async fn listen((host, port): (IpAddr, u16), state: AppState) -> Result<()> {
    let socket = TcpListener::bind((host, port))
        .await
        .context("While binding to websocket port")?;
    let mut incoming = socket.incoming();

    while let Some(stream) = incoming.next().await {
        let stream = stream.context("While accepting an incoming connection")?;

        let stream_state = state.clone();
        spawn(async move {
            let mut callback = ServerCallback {
                state: stream_state.clone(),
                user: None,
            };
            let ws_stream = match accept_hdr_async(stream, &mut callback).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        "Got an error validating an incoming websocket connection: {}",
                        e
                    );
                    return;
                }
            };
            let user = match callback.user {
                Some(u) => u,
                None => {
                    error!("Incoming websocket validation passed, but it didn't set a `user` value. This is a bug.");
                    return;
                }
            };
            match handle_connection(ws_stream, stream_state, user).await {
                Ok(()) => (),
                Err(e) => error!("Got an error while handling a websocket connection: {}", e),
            }
        });
    }

    Ok(())
}
