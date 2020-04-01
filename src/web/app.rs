use tide::{IntoResponse, Request, Response};

use super::AppState;

pub async fn home_page(_req: Request<AppState>) -> Response {
    "TODO: home_page".into_response()
}
