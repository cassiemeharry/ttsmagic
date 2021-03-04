use anyhow::{anyhow, ensure, Context};
use serde::Deserialize;
use std::borrow::Cow;
use tide::{
    http::{
        headers::{HOST, LOCATION},
        StatusCode,
    },
    Redirect, Request, Response, Result,
};
use url::Url;

use super::AppState;
use crate::web::session::{SessionClearExt, SessionSetExt};
use crate::{
    user::User,
    web::{session::Session, AnyhowTideCompat},
};

const OPENID_EXT_SREG: &'static str = "http://openid.net/extensions/sreg/1.1";
const OPENID_IDENTIFIER_SELECT: &'static str = "http://specs.openid.net/auth/2.0/identifier_select";
const OPENID_PROTOCOL_VERSION: &'static str = "http://specs.openid.net/auth/2.0";
const STEAM_OPENID_ENDPOINT: &'static str = "https://steamcommunity.com/openid/login";

fn return_to_url<T>(req: &Request<T>) -> anyhow::Result<Url> {
    let this_uri = req.url();
    debug!("Starting Steam login from {}", this_uri);
    let host_str = {
        let real_host = this_uri
            .host_str()
            .or_else(|| {
                req.header(&HOST)
                    .and_then(|vals| vals.get(0))
                    .map(|hv| hv.as_str())
            })
            .unwrap_or("ttsmagic.cards");
        if real_host == "0.0.0.0" {
            "ttsmagic.cards"
        } else {
            real_host
        }
    };
    let scheme = if host_str == "ttsmagic.cards" {
        "https"
    } else {
        this_uri.scheme()
    };
    let realm = format!("{}://{}", scheme, host_str);
    let url = Url::parse(&format!("{}/steam/complete/", realm))?;
    Ok(url)
}

pub async fn begin_login(req: Request<AppState>) -> Result {
    let rt_url = return_to_url(&req).tide_compat()?;
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
    let resp = Redirect::temporary(login_redirect_uri).into();
    Ok(resp)
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
    fn validate<State>(&self, req: &Request<State>) -> anyhow::Result<u64> {
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

pub async fn handle_redirect(request: Request<AppState>) -> tide::Result {
    let openid_response_str: &str = request.url().query().unwrap_or("");
    let openid_response: OpenIDResponse = serde_qs::from_str(openid_response_str)
        .map_err(|e| anyhow!("Failed to parse OpenID response from Steam: {}", e))
        .tide_compat()?;
    debug!("Got Steam OpenID response: {:#?}", openid_response);
    let steam_id = openid_response.validate(&request).tide_compat()?;
    let state = request.state();
    let mut db = &state.db_pool;
    let mut redis_conn = state
        .redis
        .get_async_connection()
        .await
        .context("Failed to create Redis connection after successful Steam verification")
        .tide_compat()?;
    let user = User::steam_login(&mut db, steam_id)
        .await
        .context("Failed to create Steam login after successful verification")
        .tide_compat()?;
    let new_session = Session::new(&mut db, user.id).await.tide_compat()?;
    let mut response = Response::new(StatusCode::TemporaryRedirect);
    response.insert_header(LOCATION, "/");
    response
        .set_session(&mut redis_conn, new_session)
        .await
        .context("Failed to set session after successful Steam login")
        .tide_compat()?;

    Ok(response)
}

pub async fn logout(_req: Request<AppState>) -> tide::Result {
    let mut response: Response = Redirect::temporary("/").into();
    response.clear_session().await.tide_compat()?;
    Ok(response)
}
