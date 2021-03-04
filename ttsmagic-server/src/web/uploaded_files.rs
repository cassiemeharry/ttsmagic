use tide::{Redirect, Request, Response, StatusCode};

use crate::{files::MediaFile, web::AppState};

pub async fn get(req: Request<AppState>) -> tide::Result {
    let path = req.param("path").unwrap();
    match MediaFile::get_internal_url(path).await {
        Some(url) => Ok(Redirect::temporary(url).into()),
        None => Ok(Response::new(StatusCode::NotFound)),
    }
}
