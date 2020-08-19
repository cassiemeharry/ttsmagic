use async_std::{
    net::{IpAddr, TcpListener, TcpStream},
    prelude::*,
    sync::Arc,
    task::{block_on, spawn},
};
// use async_std_tokio_compat::*;
use anyhow::{anyhow, ensure, Context, Result};
use async_tungstenite::{
    accept_hdr_async,
    tungstenite::{self, Message},
    WebSocketStream,
};
use futures::{
    channel::mpsc,
    future::BoxFuture,
    sink::{Sink, SinkExt as _},
};
use http_0_2::status::StatusCode;
use redis::AsyncCommands;
use sqlx::{Executor, Postgres};
use ttsmagic_types::{frontend_to_server as f2s, server_to_frontend as s2f};

use crate::{
    deck::{get_decks_for_user, Deck},
    notify,
    scryfall::api::ScryfallApi,
    user::User,
    utils::AsyncStdStreamWrapper,
    web::{session::SessionGetExt as _, AppState},
};

trait MessageSendExt {
    fn send<'a, S>(self, stream: &'a mut S) -> BoxFuture<'a, Result<()>>
    where
        S: Sink<Message> + Send + Unpin,
        <S as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static;
}

impl MessageSendExt for s2f::ServerToFrontendMessage {
    fn send<'a, S>(self, stream: &'a mut S) -> BoxFuture<'a, Result<()>>
    where
        S: Sink<Message> + Send + Unpin,
        <S as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
    {
        Box::pin(async move {
            let encoded = serde_json::to_string(&self)?;
            let msg = Message::Text(encoded);
            let () = stream.send(msg).await?;
            Ok(())
        })
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
    let (handle_sink, handle_stream) = mpsc::channel::<s2f::ServerToFrontendMessage>(0);
    let mut handle_stream = AsyncStdStreamWrapper::new(handle_stream);
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
                    Some(Ok(m)) => m,
                    Some(Err(e)) => s2f::Notification::Error(
                        s2f::Error {
                            user_message: "Internal error parsing notification message".to_string(),
                            details: Some(format!("{:?}", e)),
                        },
                    ),
                    None => {
                        warn!("Looks like Redis disconnected");
                        continue;
                    },
                };
                debug!("Got a message from Redis pubsub: {:?}", redis_msg);
                let msg = s2f::ServerToFrontendMessage::Notification(redis_msg);
                msg.send(&mut ws_stream).await?;
            },
            outbound_msg_opt = handle_stream.next() => {
                if let Some(outbound_msg) = outbound_msg_opt {
                    debug!("Got outbound message from handle_stream: {:?}", outbound_msg);
                    outbound_msg.send(&mut ws_stream).await?;
                }
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
                let parsed_message: f2s::FrontendToServerMessage = match ws_msg {
                    Message::Text(string) => serde_json::from_str(string.as_str())?,
                    Message::Binary(vec) => serde_json::from_slice(vec.as_slice())?,
                    Message::Ping(contents) => {
                        ws_stream.send(Message::Pong(contents)).await?;
                        continue;
                    }
                    Message::Pong(contents) => {
                        continue;
                    }
                    Message::Close(close_msg) => {
                        debug!("Got close message: {:?}", close_msg);
                        break;
                    }
                };
                let user = user.clone();
                let api = state.scryfall_api.clone();
                let mut db_conn = state.db_pool.acquire().await?;
                let mut redis_conn = state
                    .redis
                    .get_async_connection()
                    .await
                    .context("While getting Redis to render deck at request of websocket")?;
                let handle_sink_1 = handle_sink.clone();
                let mut handle_sink_2 = handle_sink.clone();
                spawn(async move {
                    let handle_result = handle_incoming_message(
                        user,
                        api,
                        db_conn,
                        redis_conn,
                        handle_sink_1,
                        parsed_message,
                    ).await;
                    match handle_result {
                        Ok(()) => (),
                        Err(e) => {
                            let error = s2f::Error {
                                user_message: format!("{}", e),
                                details: Some(format!("{:?}", e)),
                            };
                            let msg = s2f::ServerToFrontendMessage::FatalError(error);
                            match handle_sink_2.send(msg).await {
                                Ok(()) => (),
                                Err(e2) => error!("Failed to send the following error message to the WS client because of {}: {:?}", e2, e),
                            };
                        }
                    }
                });
            },
        }
    }
    Ok(())
}

async fn handle_incoming_message(
    user: User,
    api: Arc<ScryfallApi>,
    mut db_conn: impl Executor<Database = Postgres> + Send + 'static,
    mut redis_conn: impl AsyncCommands + 'static,
    mut handle_sink: mpsc::Sender<s2f::ServerToFrontendMessage>,
    msg: f2s::FrontendToServerMessage,
) -> Result<()> {
    match msg {
        f2s::FrontendToServerMessage::DeleteDeck { id } => {
            let deck: Deck = Deck::get_by_id(&mut db_conn, id)
                .await?
                .ok_or_else(|| anyhow!("Invalid deck ID"))?;
            ensure!(
                deck.user_id == user.id,
                "Invalid deck ID (that doesn't belong to you)"
            );
            deck.delete(&mut db_conn, &mut redis_conn).await?;
        }
        f2s::FrontendToServerMessage::GetDecks => {
            let decks = get_decks_for_user(&mut db_conn, user.id).await?;
            let msg = s2f::ServerToFrontendMessage::DeckList { decks };
            handle_sink.send(msg).await?;
        }
        f2s::FrontendToServerMessage::RenderDeck { url } => {
            async_std::task::spawn_blocking::<_, Result<()>>(move || {
                block_on(async move {
                    let mut deck =
                        crate::deck::load_deck(&mut db_conn, &mut redis_conn, &user, url)
                            .await
                            .context("Loading deck at request of websocket")?;
                    deck.render(api, &mut db_conn, &mut redis_conn).await?;
                    Ok(())
                })
            })
            .await?;
        }
    };
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
