use async_std::{io::Cursor, path::Path};
use tide::{Request, Response};

use super::AppState;
use crate::{
    files::StaticFiles,
    web::session::{SessionGetExt, SessionSetExt},
};

pub async fn home_page(req: Request<AppState>) -> Response {
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
    Response::new(200).body(contents).set_mime(html_mime)
}

pub async fn static_files(req: Request<AppState>) -> Response {
    let path_param = req.param::<String>("path").unwrap();
    let path = Path::new(&path_param);
    let mime_type = match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("css") => "text/css".parse().unwrap(),
        Some("js") => "text/javascript".parse().unwrap(),
        _ => "application/octet-stream".parse().unwrap(),
    };
    let contents = match StaticFiles::get(&path_param) {
        Some(cow) => Cursor::new(cow),
        None => return Response::new(404),
    };
    Response::new(200).body(contents).set_mime(mime_type)
}

pub async fn demo_login(req: Request<AppState>) -> Response {
    let state = req.state().clone();
    let mut db = &state.db_pool;
    let demo_user = crate::user::User::get_or_create_demo_user(&mut db)
        .await
        .expect("Failed to get demo user in  web::app::demo_login view");
    let session = crate::web::session::Session::new(demo_user);
    let mut resp = Response::new(302).set_header("Location", "/");
    let mut redis_conn = state
        .redis
        .get_async_connection()
        .await
        .expect("Failed to connect to redis in web::app::demo_login view");
    resp.set_session(&mut redis_conn, session)
        .await
        .expect("Failed to set session in web::app::demo_login view");
    resp
}
