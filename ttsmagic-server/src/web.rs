use anyhow::Result;
use async_std::{net::IpAddr, path::PathBuf, sync::Arc};
use sentry::integrations::anyhow::capture_anyhow;

mod app;
mod deck;
mod session;
mod steam;
mod uploaded_files;
mod ws;

use crate::scryfall::api::ScryfallApi;

pub trait SurfErrorCompat<T> {
    fn surf_compat(self) -> anyhow::Result<T>;
}

impl<T> SurfErrorCompat<T> for std::result::Result<T, surf::Error> {
    fn surf_compat(self) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::Error::msg(e))
    }
}

pub type AppState = Arc<AppStateInner>;

#[derive(Debug)]
pub struct AppStateInner {
    scryfall_api: Arc<ScryfallApi>,
    db_pool: sqlx::PgPool,
    redis: redis::Client,
    root: PathBuf,
}

async fn sentry_middleware(mut resp: tide::Response) -> tide::Result {
    if let Some(error) = resp.take_error() {
        let status = error.status();
        let inner = error.into_inner();
        error!("Tide view function returned {}: {}", status, inner);
        capture_anyhow(&inner);
        let error = tide::Error::new(status, inner);
        resp.set_error(error);
    }
    Ok(resp)
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

    app.with(tide::utils::After(sentry_middleware));
    app.with(tide::log::LogMiddleware::new());
    app.with(session::SessionMiddleware::new());

    app.at("/").get(app::home_page);
    app.at("/decks/:deck_id").get(deck::download_deck_json);
    app.at("/static/*path").get(app::static_files);
    app.at("/files/*path").get(uploaded_files::get);
    #[cfg(debug_assertions)]
    app.at("/demo-login/").get(app::demo_login);
    app.at("/steam/login/").get(steam::begin_login);
    app.at("/steam/complete/").get(steam::handle_redirect);
    app.at("/logout/").get(steam::logout);

    info!(
        "Listening on {}:{} (websocket on port {})",
        host, web_port, ws_port
    );

    let listener = async_std::net::TcpListener::bind((host, web_port)).await?;
    let app_listen = app.listen(listener);
    pin_mut!(app_listen);

    let ws_listen = ws::listen((host, ws_port), state);
    pin_mut!(ws_listen);

    let (_app_result, _ws_result) = futures::join!(app_listen, ws_listen);
    Ok(())
}
