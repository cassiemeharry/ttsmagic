use std::str::FromStr;
use tide::{Request, Response};
use ttsmagic_types::DeckId;

use super::AppState;
use crate::{deck::Deck, web::session::SessionGetExt};

pub async fn download_deck_json(req: Request<AppState>) -> Response {
    macro_rules! ensure_404 {
        ($cond:expr, $msg:literal, $($arg:expr),* $(,)*) => {
            if !$cond {
                error!($msg, $($arg,)*);
                return Response::new(404);
            }
        }
    };
    macro_rules! opt_404 {
        ($opt:expr) => {
            match $opt {
                Some(x) => x,
                None => return Response::new(404),
            }
        };
    };
    macro_rules! result_404 {
        ($result:expr, $msg:literal, $($arg:expr),* $(,)*) => {
            match $result {
                Ok(x) => x,
                Err(e) => {
                    error!($msg, $($arg,)* e);
                    return Response::new(404);
                }
            }
        };
    };
    let _404 = Response::new(404);
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
    let mut db = result_404!(
        state.db_pool.acquire().await,
        "Failed to open DB connection: {}",
    );
    let deck_opt = result_404!(
        Deck::get_by_id(&mut db, deck_id).await,
        "Failed to look up deck with ID {}: {}",
        deck_id,
    );
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
    Response::new(200)
        .body_string(rendered_json)
        .set_mime(json_mime)
        .set_header(
            "Content-Disposition",
            format!(
                "attachment; filename=\"{}.json\"",
                deck.title.replace('"', "'")
            ),
        )
}
