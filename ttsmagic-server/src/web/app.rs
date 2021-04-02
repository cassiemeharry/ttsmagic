use async_std::{io::Cursor, path::Path};
use tide::{http::mime::Mime, Request, Response, Result, StatusCode};

use super::AppState;
use crate::{files::StaticFiles, web::session::SessionGetExt};

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
    let size_hint = Some(contents_cow.len());
    let contents = Cursor::new(contents_cow);
    let html_mime: Mime = "text/html; charset=utf-8".parse().unwrap();
    let body = tide::Body::from_reader(contents, size_hint);
    let mut resp = Response::new(StatusCode::Ok);
    resp.set_body(body);
    resp.set_content_type(html_mime);
    Ok(resp)
}

pub async fn static_files(req: Request<AppState>) -> Result {
    let path_param = req.param("path").unwrap();
    let path = Path::new(path_param);
    let mime_type: Mime = match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("css") => "text/css".parse().unwrap(),
        Some("js") => "text/javascript".parse().unwrap(),
        Some("wasm") => "application/wasm".parse().unwrap(),
        _ => "application/octet-stream".parse().unwrap(),
    };
    info!("Loading static file {}...", path_param);
    let (contents, size_hint) = match StaticFiles::get(&path_param) {
        Some(cow) => {
            let size = cow.len();
            (Cursor::new(cow), Some(size))
        }
        None => return Ok(Response::new(StatusCode::NotFound)),
    };
    let mut resp = Response::new(StatusCode::Ok);
    let body = tide::Body::from_reader(contents, size_hint);
    resp.set_body(body);
    resp.set_content_type(mime_type);
    Ok(resp)
}

#[allow(unused)]
#[cfg(debug_assertions)]
pub async fn demo_login(req: Request<AppState>) -> Result {
    use crate::web::session::SessionSetExt;
    use tide::{http::headers::HeaderName, Redirect};

    let state = req.state().clone();
    let mut db_conn = state.db_pool.acquire().await?;
    let db_conn_1: &'_ mut sqlx::PgConnection = &mut *db_conn;
    let demo_user = crate::user::User::get_or_create_demo_user(db_conn_1).await?;
    let session = crate::web::session::Session::new(&mut *db_conn, demo_user.id).await?;
    let mut resp: Response = Redirect::new("/").into();
    let mut redis_conn = state.redis.get_async_connection().await?;
    resp.set_session(&mut redis_conn, session).await?;
    Ok(resp)
}
