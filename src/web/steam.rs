use failure::{Error, ResultExt as _};
use std::convert::TryFrom;
use steam_auth::{Redirector, Verifier};
use tide::{
    http::{status::StatusCode, Request as HttpRequest},
    IntoResponse, Request, Response,
};

use super::AppState;

pub async fn begin_login(_req: Request<AppState>) -> Response {
    let redirector: Redirector = Redirector::new("https://ttsmagic.cards", "/steam/complete/")
        .expect("Failed to create Steam redirector");
    Response::new(307).set_header("Location", redirector.url().as_str())
}

async fn handle_redirect_inner(req: Request<AppState>) -> Result<String, Error> {
    use async_std::io::Cursor;
    let login_data = req.query().map_err(super::TideError::from)?;
    debug!("Got login data from steam: {:?}", login_data);
    let (req, verifier) = Verifier::from_parsed(login_data).compat()?;
    let (req_parts, req_body_owned) = req.into_parts();
    let req_body: Box<Cursor<Vec<u8>>> = Box::new(Cursor::new(req_body_owned));
    let req: HttpRequest<Box<Cursor<Vec<u8>>>> = HttpRequest::from_parts(req_parts, req_body);
    let req = surf::Request::<http_client::isahc::IsahcClient>::try_from(req)?;
    debug!("Verifying login data with Steam...");
    let mut response = req.await.map_err(Error::from_boxed_compat)?;
    let response_body = response
        .body_string()
        .await
        .map_err(Error::from_boxed_compat)?;
    let steam_id = verifier.verify_response(response_body)?;
    Ok(format!("Got Steam ID {}!", steam_id))
}

pub async fn handle_redirect(req: Request<AppState>) -> Response {
    match handle_redirect_inner(req).await {
        Ok(r) => r.into_response(),
        Err(e) => format!("Error verifying login data with Steam: {}", e)
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
    }
}
