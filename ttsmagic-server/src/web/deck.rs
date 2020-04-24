use std::str::FromStr;
use tide::{http::headers::HeaderName, Request, Response, Result, StatusCode};
use ttsmagic_types::DeckId;

use super::AppState;
use crate::{
    deck::Deck,
    web::{session::SessionGetExt, AnyhowTideCompat},
};

pub async fn download_deck_json(req: Request<AppState>) -> Result {
    macro_rules! ensure_404 {
        ($cond:expr, $msg:literal, $($arg:expr),* $(,)*) => {
            if !$cond {
                error!($msg, $($arg,)*);
                return Err(tide::Error::from_str(StatusCode::NotFound, "Invalid deck"));
            }
        }
    };
    macro_rules! opt_404 {
        ($opt:expr) => {
            match $opt {
                Some(x) => x,
                None => return Err(tide::Error::from_str(StatusCode::NotFound, "Invalid deck")),
            }
        };
    };
    macro_rules! result_404 {
        ($result:expr, $msg:literal, $($arg:expr),* $(,)*) => {
            match $result {
                Ok(x) => x,
                Err(e) => {
                    error!($msg, $($arg,)* e);
                    return Err(tide::Error::from_str(StatusCode::NotFound, "Invalid deck"));
                }
            }
        };
    };
    let user = {
        let session_opt_future = req.get_session();
        let session_opt = session_opt_future.await;
        opt_404!(session_opt.and_then(|s| s.user))
    };

    let deck_id: DeckId = {
        let param: String = req.param("deck_id").unwrap();
        ensure_404!(
            param.ends_with(".json") && param.len() > 5,
            "Invalid deck ID (expected something like {:?}), got {:?}",
            "$UUID.json",
            param,
        );
        let raw_id = &param[..param.len() - 5];
        result_404!(
            DeckId::from_str(raw_id),
            "Failed to parse deck ID from {:?} in download_deck_json view: {}",
            raw_id,
        )
    };
    let state = req.state();
    let mut db = state.db_pool.acquire().await?;
    let deck_opt = Deck::get_by_id(&mut db, deck_id).await.tide_compat()?;
    let mut deck = opt_404!(deck_opt);
    ensure_404!(
        deck.user_id == user.id,
        "Attempted to access another user's deck (current user is {}, deck's owner is {})",
        user.id,
        deck.user_id,
    );
    let deck_json = match deck.rendered_json {
        Some(j) => j,
        None => {
            let mut redis_conn = result_404!(
                state.redis.get_async_connection().await,
                "Failed to create Redis connection: {}",
            );
            let rendered_result = deck
                .render(
                    state.scryfall_api.clone(),
                    &mut db,
                    &mut redis_conn,
                    &state.root,
                )
                .await;
            let rendered = result_404!(rendered_result, "Failed to render deck {}: {}", deck.id);
            rendered.json_description
        }
    };

    let json_mime = FromStr::from_str("application/json".into()).unwrap();
    let rendered_json = serde_json::to_string_pretty(&deck_json).unwrap();
    let resp = Response::new(StatusCode::Ok)
        .body_string(rendered_json)
        .set_mime(json_mime)
        .set_header(
            HeaderName::from_ascii(b"Content-Disposition".to_vec()).unwrap(),
            format!(
                "attachment; filename=\"{}.json\"",
                deck.title.replace('"', "'")
            ),
        );
    Ok(resp)
}
