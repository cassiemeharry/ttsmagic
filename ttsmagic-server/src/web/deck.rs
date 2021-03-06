use std::str::FromStr;
use tide::{
    http::{headers::HeaderName, mime::Mime},
    Request, Response, Result, StatusCode,
};
use ttsmagic_types::DeckId;

use super::AppState;
use crate::{deck::Deck, web::session::SessionGetExt};

pub async fn download_deck_json(req: Request<AppState>) -> Result {
    macro_rules! ensure_404 {
        ($cond:expr, $msg:literal, $($arg:expr),* $(,)*) => {
            if !$cond {
                error!($msg, $($arg,)*);
                return Err(tide::Error::from_str(StatusCode::NotFound, "Invalid deck"));
            }
        }
    }
    macro_rules! opt_404 {
        ($opt:expr) => {
            match $opt {
                Some(x) => x,
                None => return Err(tide::Error::from_str(StatusCode::NotFound, "Invalid deck")),
            }
        };
    }
    macro_rules! result_404 {
        ($result:expr, $msg:literal, $($arg:expr),* $(,)*) => {
            match $result {
                Ok(x) => x,
                Err(e) => {
                    error!($msg, $($arg,)* e);
                    let e = anyhow::Error::from(e);
                    let tide_error = tide::Error::new(StatusCode::NotFound, e.context("Invalid deck"));
                    return Err(tide_error);
                }
            }
        };
    }
    let user = {
        let session_opt_future = req.get_session();
        let session_opt = session_opt_future.await;
        opt_404!(session_opt.and_then(|s| s.user))
    };

    let deck_id: DeckId = {
        let param: &str = req.param("deck_id").unwrap();
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
    let mut db_conn = state.db_pool.acquire().await?;
    let deck_opt = Deck::get_by_id(&mut *db_conn, deck_id).await?;
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
                .render(state.scryfall_api.clone(), &mut *db_conn, &mut redis_conn)
                .await;
            let rendered = result_404!(rendered_result, "Failed to render deck {}: {}", deck.id);
            rendered.json_description
        }
    };

    let json_mime: Mime = "application/json".parse().unwrap();
    let rendered_json = serde_json::to_string_pretty(&deck_json).unwrap();
    let mut resp = Response::new(StatusCode::Ok);
    resp.set_body(rendered_json);
    resp.set_content_type(json_mime);
    resp.insert_header(
        HeaderName::from_bytes(b"Content-Disposition".to_vec()).unwrap(),
        format!(
            "attachment; filename=\"{}.json\"",
            deck.title.replace('"', "'")
        ),
    );
    Ok(resp)
}
