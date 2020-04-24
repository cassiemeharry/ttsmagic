use async_std::{io::Cursor, path::Path};
use tide::{http::headers::HeaderName, Request, Response, Result, StatusCode};

use super::AppState;
use crate::{
    files::StaticFiles,
    web::{
        session::{SessionGetExt, SessionSetExt},
        AnyhowTideCompat,
    },
};

pub async fn home_page(req: Request<AppState>) -> Result {
    let get_session_future = req.get_session();
    let session_opt = get_session_future.await;
    let user_opt = session_opt.and_then(|s| s.user);
    let html_page = match user_opt {
        Some(user) => {
            debug!("Got logged in page load for user {}", user);
            "logged_in.html"
        }
        None => {
            debug!("Got logged out page load");
            "logged_out.html"
        }
    };
    let contents_cow = StaticFiles::get(html_page).unwrap();
    let contents = Cursor::new(contents_cow);
    let html_mime = "text/html; charset=utf-8".parse().unwrap();
    let resp = Response::new(StatusCode::Ok)
        .body(contents)
        .set_mime(html_mime);
    Ok(resp)
}

pub async fn static_files(req: Request<AppState>) -> Result {
    let path_param = req.param::<String>("path").unwrap();
    let path = Path::new(&path_param);
    let mime_type = match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("css") => "text/css".parse().unwrap(),
        Some("js") => "text/javascript".parse().unwrap(),
        Some("wasm") => "application/wasm".parse().unwrap(),
        _ => "application/octet-stream".parse().unwrap(),
    };
    info!("Loading static file {}...", path_param);
    let contents = match StaticFiles::get(&path_param) {
        Some(cow) => Cursor::new(cow),
        None => return Ok(Response::new(StatusCode::NotFound)),
    };
    let resp = Response::new(StatusCode::Ok)
        .body(contents)
        .set_mime(mime_type);
    Ok(resp)
}

pub async fn demo_login(req: Request<AppState>) -> Result {
    let state = req.state().clone();
    let mut db = &state.db_pool;
    let demo_user = crate::user::User::get_or_create_demo_user(&mut db)
        .await
        .tide_compat()?;
    let session = crate::web::session::Session::new(demo_user);
    let mut resp = Response::new(StatusCode::Found).set_header(
        HeaderName::from_ascii(b"Location".to_vec()).unwrap(),
        "/beta/",
    );
    let mut redis_conn = state.redis.get_async_connection().await?;
    resp.set_session(&mut redis_conn, session)
        .await
        .tide_compat()?;
    Ok(resp)
}
