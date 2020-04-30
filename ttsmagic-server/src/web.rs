use anyhow::Result;
use async_std::{net::IpAddr, path::PathBuf, sync::Arc};
use thiserror::Error;

mod app;
mod deck;
mod session;
mod steam;
mod ws;

use crate::scryfall::api::ScryfallApi;

pub trait AnyhowTideCompat<T> {
    fn tide_compat(self) -> std::result::Result<T, AnyhowTideCompatError>;
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct AnyhowTideCompatError(anyhow::Error);

impl<T, E: Into<anyhow::Error>> AnyhowTideCompat<T> for std::result::Result<T, E> {
    fn tide_compat(self) -> std::result::Result<T, AnyhowTideCompatError> {
        self.map_err(|e| AnyhowTideCompatError(e.into()))
    }
}

pub type AppState = Arc<AppStateInner>;

#[derive(Debug)]
pub struct AppStateInner {
    #[allow(unused)]
    scryfall_api: Arc<ScryfallApi>,
    #[allow(unused)]
    db_pool: sqlx::PgPool,
    #[allow(unused)]
    redis: redis::Client,
    #[allow(unused)]
    root: PathBuf,
}

pub async fn run_server(
    scryfall_api: Arc<ScryfallApi>,
    db_pool: sqlx::PgPool,
    redis: redis::Client,
    root: PathBuf,
    host: IpAddr,
    web_port: u16,
    ws_port: u16,
) -> Result<()> {
    let state = Arc::new(AppStateInner {
        scryfall_api,
        db_pool,
        redis,
        root,
    });
    let mut app = tide::with_state(state.clone());

    app.middleware(tide::log::LogMiddleware::new());
    app.middleware(session::SessionMiddleware::new());

    app.at("/").get(app::home_page);
    app.at("/decks/:deck_id").get(deck::download_deck_json);
    app.at("/static/*path").get(app::static_files);
    app.at("/demo-login/").get(app::demo_login);
    app.at("/steam/login/").get(steam::begin_login);
    app.at("/steam/complete/").get(steam::handle_redirect);
    app.at("/logout/").get(steam::logout);

    info!(
        "Listening on {}:{} (websocket on port {})",
        host, web_port, ws_port
    );

    let app_listen = app.listen((host, web_port));
    pin_mut!(app_listen);

    let ws_listen = ws::listen((host, ws_port), state);
    pin_mut!(ws_listen);

    let (_app_result, _ws_result) = futures::join!(app_listen, ws_listen);
    Ok(())
}