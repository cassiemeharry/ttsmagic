use tide::{Request, Response, StatusCode};

use crate::{files::MediaFile, web::AppState};

pub async fn get(req: Request<AppState>) -> tide::Result {
    let path = req.param::<String>("path").unwrap();
    match MediaFile::get_internal_url(&path).await {
        Ok(Some(url)) => Ok(Response::redirect_temporary(url)),
        Ok(None) => Ok(Response::new(StatusCode::NotFound)),
        Err(e) => {
            error!("Failed to look up media file {}: {:?}", path, e);
            Ok(Response::new(StatusCode::InternalServerError))
        }
    }
}
