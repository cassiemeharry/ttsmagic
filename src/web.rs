use async_std::{net::IpAddr, path::PathBuf, sync::Arc};
use failure::{Error, Fail};

mod app;
mod steam;

use crate::scryfall::api::ScryfallApi;

#[derive(Debug, Fail)]
#[fail(display = "Tide error: {:?}", inner)]
struct TideError {
    inner: std::sync::Mutex<tide::Error>,
}

impl From<tide::Error> for TideError {
    fn from(inner: tide::Error) -> TideError {
        let inner = std::sync::Mutex::new(inner);
        TideError { inner }
    }
}

pub struct AppState {
    #[allow(unused)]
    scryfall_api: Arc<ScryfallApi>,
    #[allow(unused)]
    db_pool: sqlx::PgPool,
    #[allow(unused)]
    root: PathBuf,
}

pub async fn run_server(
    scryfall_api: Arc<ScryfallApi>,
    db_pool: sqlx::PgPool,
    root: PathBuf,
    host: IpAddr,
    port: u16,
) -> Result<(), Error> {
    let state = AppState {
        scryfall_api,
        db_pool,
        root,
    };
    let mut app = tide::with_state(state);
    app.at("/").get(app::home_page);
    app.at("/steam/login/").get(steam::begin_login);
    app.at("/steam/complete/").get(steam::handle_redirect);

    println!("Listening on {}:{}", host, port);
    app.listen((host, port)).await?;
    Ok(())
}
