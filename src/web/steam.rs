use anyhow::{anyhow, ensure, Context, Result};
use serde::Deserialize;
use std::borrow::Cow;
use tide::{http::status::StatusCode, IntoResponse, Request, Response};
use url::Url;

use super::AppState;
use crate::{
    user::User,
    web::session::{Session, SessionSetExt},
};

// mod secrets;

const OPENID_EXT_SREG: &'static str = "http://openid.net/extensions/sreg/1.1";
const OPENID_IDENTIFIER_SELECT: &'static str = "http://specs.openid.net/auth/2.0/identifier_select";
const OPENID_PROTOCOL_VERSION: &'static str = "http://specs.openid.net/auth/2.0";
const STEAM_OPENID_ENDPOINT: &'static str = "https://steamcommunity.com/openid/login";

fn return_to_url<T>(req: &Request<T>) -> Result<Url> {
    let this_uri = req.uri();
    debug!("Starting Steam login from {}", this_uri);
    let realm = format!(
        "{}://{}",
        this_uri.scheme_str().unwrap_or("http"),
        this_uri
            .authority_part()
            .map(|a| a.as_str())
            .or_else(|| req.header("host"))
            .unwrap_or("ttsmagic.cards"),
    );
    let url = Url::parse(&format!("{}/beta/steam/complete/", realm))?;
    Ok(url)
}

async fn begin_login_inner(req: Request<AppState>) -> Result<Response> {
    let rt_url = return_to_url(&req)?;
    let return_to = format!("{}", rt_url);
    let realm = format!("{}://{}/", rt_url.scheme(), rt_url.host_str().unwrap());
    let params = &[
        ("openid.claimed_id", OPENID_IDENTIFIER_SELECT),
        ("openid.identity", OPENID_IDENTIFIER_SELECT),
        ("openid.mode", "checkid_setup"),
        ("openid.ns", OPENID_PROTOCOL_VERSION),
        ("openid.ns.sreg", OPENID_EXT_SREG),
        ("openid.realm", &realm),
        ("openid.return_to", &return_to),
        ("openid.sreg.optional", "fullname,email,nickname"),
    ];
    let login_redirect_uri = Url::parse_with_params(STEAM_OPENID_ENDPOINT, params)?;
    Ok(Response::new(307).set_header("Location", login_redirect_uri))
}

pub async fn begin_login(req: Request<AppState>) -> Response {
    match begin_login_inner(req).await {
        Ok(r) => r,
        Err(e) => format!("Error creating Steam login redirect: {}", e)
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct OpenIDResponse<'a> {
    #[serde(rename = "openid.ns")]
    ns: Cow<'a, str>,
    #[serde(rename = "openid.mode")]
    mode: Cow<'a, str>,
    #[serde(rename = "openid.op_endpoint")]
    op_endpoint: Cow<'a, str>,
    #[serde(rename = "openid.claimed_id")]
    claimed_id: Cow<'a, str>,
    #[serde(rename = "openid.identity")]
    identity: Cow<'a, str>,
    #[serde(rename = "openid.return_to")]
    return_to: Cow<'a, str>,
    #[serde(rename = "openid.response_nonce")]
    response_nonce: Cow<'a, str>,
    #[serde(rename = "openid.assoc_handle")]
    assoc_handle: Cow<'a, str>,
    #[serde(rename = "openid.signed")]
    signed: Cow<'a, str>,
    #[serde(rename = "openid.sig")]
    sig: Cow<'a, str>,
}

impl<'a> OpenIDResponse<'a> {
    fn validate<State>(&self, req: &Request<State>) -> Result<u64> {
        // See https://openid.net/specs/openid-authentication-2_0.html#verification
        // 11.1: Verifying the Return URL
        let expected_rt_url = format!("{}", return_to_url(req)?);
        ensure!(
            &self.return_to == &expected_rt_url,
            "return_to mismatch: got {:?}, expected {:?}",
            self.return_to,
            expected_rt_url
        );

        // 11.2: Verifying Discovered Information
        ensure!(
            self.ns == OPENID_PROTOCOL_VERSION,
            "Invalid openid.ns value"
        );
        ensure!(self.mode == "id_res", "Invalid openid.mode value");
        ensure!(
            self.op_endpoint == STEAM_OPENID_ENDPOINT,
            "Invalid openid.op_endpoint value"
        );
        ensure!(
            self.claimed_id == self.identity,
            "openid.claimed_id differs from openid.identity"
        );
        ensure!(
            self.claimed_id
                .starts_with("https://steamcommunity.com/openid/id/"),
            "openid.claimed_id is not a Steam OpenID URL"
        );

        let identity_url = Url::parse(&self.claimed_id)?;
        let identity_url_path_segments = identity_url
            .path_segments()
            .unwrap_or("".split('/'))
            .collect::<Vec<&str>>();
        let steam_id: u64 = match identity_url_path_segments.as_slice() {
            ["openid", "id", steam_id_str] => steam_id_str.parse()?,
            _ => anyhow::bail!("Invalid path in claimed_id URL {}", identity_url),
        };

        // 11.3: Checking the Nonce
        // TODO
        warn!(
            "Skipping sign-in nonce verification of OpenID login for Steam user {}",
            steam_id
        );

        // 11.4: Verifying Signatures
        // TODO
        warn!(
            "Skipping sign-in signature verification of OpenID login for Steam user {}",
            steam_id
        );

        Ok(steam_id)
    }
}

async fn handle_redirect_inner(request: Request<AppState>) -> Result<impl IntoResponse> {
    let openid_response_str: &str = request.uri().query().unwrap_or("");
    let openid_response: OpenIDResponse = serde_qs::from_str(openid_response_str)
        .map_err(|e| anyhow!("Failed to parse OpenID response from Steam: {}", e))?;
    debug!("Got Steam OpenID response: {:#?}", openid_response);
    let steam_id = openid_response.validate(&request)?;
    let state = request.state();
    let mut db = &state.db_pool;
    let mut redis_conn = state
        .redis
        .get_async_connection()
        .await
        .context("Creating redis connection after successful steam verification")?;
    let user = User::steam_login(&mut db, steam_id)
        .await
        .context("Creating Steam login after successful verification")?;
    let new_session = Session::new_from_user_id(&mut db, user.id).await?;
    let mut response = Response::new(307).set_header("Location", "/beta/");
    response
        .set_session(&mut redis_conn, new_session)
        .await
        .context("Setting session after successful Steam login")?;

    Ok(response)
}

pub async fn handle_redirect(req: Request<AppState>) -> Response {
    match handle_redirect_inner(req).await {
        Ok(r) => r.into_response(),
        Err(e) => format!("Error verifying login data with Steam: {}", e)
            .with_status(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
    }
}
