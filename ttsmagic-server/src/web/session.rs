use anyhow::{anyhow, ensure, Context, Result};
use futures::future::BoxFuture;
use redis::AsyncCommands;
use ring::hmac;
use sqlx::{PgConnection, Postgres};
use std::str::FromStr;
use tide::{
    http::{headers::COOKIE, Cookie},
    Next, Request, Response,
};
use ttsmagic_types::UserId;
use uuid::Uuid;

use crate::{secrets::session_private_key, user::User, web::AppState};

#[derive(Clone, Debug)]
pub struct Session {
    pub session_id: Uuid,
    pub user: Option<User>,
}

pub const SESSION_EXPIRE_SECONDS: usize = 3 * 24 * 60 * 60;
pub const SESSION_COOKIE_NAME: &'static str = "ttsmagic-session";

impl Session {
    pub async fn new(db: &mut PgConnection, user_id: UserId) -> Result<Self> {
        let new_session_id = Uuid::new_v4();
        Self::from_user_id(db, new_session_id, user_id).await
    }

    async fn from_user_id(
        db: &mut PgConnection,
        session_id: Uuid,
        user_id: UserId,
    ) -> Result<Self> {
        let user = User::get_by_id(db, user_id)
            .await?
            .ok_or_else(|| anyhow!("No user in the database with ID {}", user_id))?;
        Ok(Session {
            session_id,
            user: Some(user),
        })
    }

    fn redis_key(&self) -> String {
        format!("ttsmagic-sessions:{}", self.session_id)
    }

    fn verify_session_id_signature(signed_cookie_value: &[u8]) -> Result<Uuid> {
        // Example UUID: d12cf21a-e09c-41d5-92bb-9a0cefbd04f4
        ensure!(
            signed_cookie_value.len() == (36 + 1 + 64),
            "Invalid cookie format: expected \"$UUID:$signature\", found len {:?}",
            signed_cookie_value.len()
        );
        let session_id_bytes: &[u8] = &signed_cookie_value[0..36];
        let session_id_str = std::str::from_utf8(session_id_bytes)?;
        let session_id: Uuid = session_id_str.parse()?;
        let sep_byte: u8 = signed_cookie_value[36];
        ensure!(
            sep_byte == (':' as u8),
            "Invalid cookie format: expected \"$UUID:$signature\", found separator byte {:?}, expected {:?}",
            sep_byte, ':' as u8,
        );
        let sig_from_cookie_hex: &[u8] = &signed_cookie_value[37..];
        let sig_from_cookie = hex::decode(sig_from_cookie_hex)?;
        ensure!(
            sig_from_cookie.len() == (256 / 8),
            "signature has invalid length"
        );

        let algo = hmac::HMAC_SHA256;
        let key = hmac::Key::new(algo, session_private_key().as_ref());
        hmac::verify(&key, session_id_bytes, &sig_from_cookie)
            .map_err(|_| anyhow!("Signature verification failed"))?;
        Ok(session_id)
    }

    fn signed_session_id(&self) -> String {
        let session_id_str = self.session_id.to_string();
        let session_id_bytes: &[u8] = session_id_str.as_bytes();
        let algo = hmac::HMAC_SHA256;
        let key = hmac::Key::new(algo, session_private_key().as_ref());
        let signature = hmac::sign(&key, session_id_bytes);
        let encoded_sig = hex::encode(signature);
        format!("{}:{}", session_id_str, encoded_sig)
    }

    async fn load_from_cache(
        db: &mut PgConnection,
        redis: &mut redis::aio::Connection,
        signed_cookie_value: &str,
    ) -> Result<Option<Self>> {
        if signed_cookie_value.is_empty() {
            return Ok(None);
        }

        let session_id = Self::verify_session_id_signature(signed_cookie_value.as_bytes())?;
        let session = Session {
            session_id,
            user: None,
        };
        let key = session.redis_key();
        match redis.get::<String, Option<String>>(key).await? {
            Some(raw_user_id) => {
                let user_id = <UserId as FromStr>::from_str(raw_user_id.as_str())?;
                let session = Self::from_user_id(db, session_id, user_id).await?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    async fn save_to_cache(&self, redis: &mut redis::aio::Connection) -> Result<()> {
        if let Some(user) = self.user.as_ref() {
            let () = redis
                .set_ex(
                    self.redis_key(),
                    user.id.to_string(),
                    SESSION_EXPIRE_SECONDS,
                )
                .await?;
        }
        Ok(())
    }

    fn make_cookie_inner(signed_session_id: String) -> Cookie<'static> {
        Cookie::build(SESSION_COOKIE_NAME, signed_session_id)
            .path("/")
            // .secure(true)
            .http_only(true)
            .max_age(time::Duration::days(7))
            .finish()
    }

    pub fn make_empty_cookie() -> Cookie<'static> {
        Self::make_cookie_inner(String::new())
    }

    pub fn make_cookie(&self) -> Cookie<'static> {
        let signed = self.signed_session_id();
        Self::make_cookie_inner(signed)
    }
}

#[test]
fn test_cookie_sig_roundtrip() {
    let session = Session {
        session_id: Uuid::new_v4(),
        user: None,
    };
    println!("session: {:?}", session);
    let cookie = session.make_cookie();
    println!("Got cookie: {}", cookie);
    let session_id = Session::verify_session_id_signature(cookie.value().as_bytes()).unwrap();
    println!("parsed session_id: {:?}", session_id);
    assert_eq!(session.session_id, session_id);
}

pub trait SessionGetExt<'a> {
    fn get_session(self) -> BoxFuture<'a, Option<Session>>;
}

pub trait SessionSetExt<'a> {
    fn set_session(
        self,
        redis: &'a mut redis::aio::Connection,
        session: Session,
    ) -> BoxFuture<'a, Result<()>>;
}

pub trait SessionClearExt<'a>: Sized {
    fn clear_session(self) -> BoxFuture<'a, Result<Self>>;
}

impl<'a> SessionGetExt<'a>
    for (
        &'a mut PgConnection,
        &'a mut redis::aio::Connection,
        &'a http::HeaderMap,
    )
{
    fn get_session(self) -> BoxFuture<'a, Option<Session>> {
        async fn inner(
            (db, redis, headers): (
                &mut PgConnection,
                &mut redis::aio::Connection,
                &http::HeaderMap,
            ),
        ) -> Result<Option<Session>> {
            let cookie_header = headers
                .get("Cookie")
                .ok_or_else(|| anyhow!("Cookie header is missing"))
                .context("Failed to get session information from http::Request")?;
            let cookie_str = cookie_header
                .to_str()
                .context("Failed to get session information from http::Request")?;
            let s_opt = from_cookie_header(&mut *db, redis, cookie_str).await?;
            Ok(s_opt)
        }
        Box::pin(async move {
            match inner(self).await {
                Ok(s_opt) => s_opt,
                Err(e) => {
                    sentry::integrations::anyhow::capture_anyhow(&e);
                    error!("Error when getting a session out of a http::Request: {}", e);
                    None
                }
            }
        })
    }
}

impl<'a> SessionGetExt<'a>
    for (
        &'a mut sqlx::pool::PoolConnection<Postgres>,
        &'a mut redis::aio::Connection,
        &'a http::HeaderMap,
    )
{
    fn get_session(self) -> BoxFuture<'a, Option<Session>> {
        let (pool_conn, redis, headers) = self;
        let db_conn: &mut PgConnection = &mut *pool_conn;
        (db_conn, redis, headers).get_session()
    }
}

impl<'a> SessionGetExt<'static> for &'a tide::Request<AppState> {
    fn get_session(self) -> BoxFuture<'static, Option<Session>> {
        let local: Option<SessionState> = self.ext::<SessionState>().cloned();
        let session_opt = local.map(|ss| ss.session);
        Box::pin(async move { session_opt })
    }
}

impl<'a> SessionSetExt<'a> for &'a mut Response {
    fn set_session(
        self,
        redis: &'a mut redis::aio::Connection,
        session: Session,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            session.save_to_cache(redis).await?;
            let cookie = session.make_cookie();
            self.insert_cookie(cookie);
            Ok(())
        })
    }
}

impl<'a> SessionClearExt<'a> for &'a mut Response {
    fn clear_session(self) -> BoxFuture<'a, Result<Self>> {
        Box::pin(async move {
            let cookie = Session::make_empty_cookie();
            self.insert_cookie(cookie);
            Ok(self)
        })
    }
}

pub async fn from_cookie_header(
    db: &mut PgConnection,
    redis: &mut redis::aio::Connection,
    header_value: &str,
) -> Result<Option<Session>> {
    for cookie_str_untrimmed in header_value.split("; ") {
        let cookie_str = cookie_str_untrimmed.trim();
        if !cookie_str.starts_with(SESSION_COOKIE_NAME) {
            continue;
        }
        let cookie = cookie::Cookie::parse(cookie_str)
            .with_context(|| format!("Failed to parse cookie header {:?}", header_value))?;
        let cookie_value: std::borrow::Cow<str> =
            percent_encoding::percent_decode(cookie.value().as_bytes()).decode_utf8()?;
        let session_opt = Session::load_from_cache(db, redis, &cookie_value)
            .await
            .with_context(|| {
                format!(
                    "Failed to load session data from cache based on cookie {:?}",
                    &cookie_value
                )
            })?;
        return Ok(session_opt);
    }
    Ok(None)
}

#[derive(Clone, Debug)]
struct SessionState {
    session: Session,
}

#[derive(Debug)]
pub struct SessionMiddleware {
    _priv: (),
}

impl SessionMiddleware {
    pub fn new() -> Self {
        SessionMiddleware { _priv: () }
    }

    async fn middleware_inner<'a>(
        mut req: Request<AppState>,
        next: Next<'a, AppState>,
    ) -> tide::Result {
        let mut session = Session {
            session_id: Uuid::new_v4(),
            user: None,
        };
        {
            let state = &req.state();
            let redis_client = &state.redis;
            let mut redis_conn = redis_client.get_async_connection().await?;
            let db_pool = &state.db_pool;
            let mut db_conn = db_pool.acquire().await?;
            let cookie_header_values = req
                .header(&COOKIE)
                .cloned()
                .unwrap_or_else(|| vec![].into_iter().collect());
            let mut cookie_session = None;
            for hv in cookie_header_values.iter() {
                match from_cookie_header(&mut *db_conn, &mut redis_conn, hv.as_str()).await {
                    Ok(s) => cookie_session = s,
                    Err(e) => {
                        sentry::integrations::anyhow::capture_anyhow(&e);
                        error!("Failed to parse cookie header {:?}: {:?}", hv.as_str(), e);
                    }
                }
            }
            if let Some(s) = cookie_session {
                session = s;
            } else {
                session.save_to_cache(&mut redis_conn).await?;
            }
        }
        // Record user info in Sentry when errors occur
        let mut _prev_scope = None;
        if let Some(user) = session.user.as_ref() {
            let hub = sentry::Hub::current();
            _prev_scope = Some(hub.push_scope());
            hub.configure_scope(|scope| {
                let sentry_user = user.into();
                scope.set_user(Some(sentry_user));
            });
        }

        let _ = req.set_ext(SessionState { session });
        let resp = next.run(req).await;
        Ok(resp)
    }
}

impl tide::Middleware<AppState> for SessionMiddleware {
    fn handle<'a, 'b, 'c>(
        &'a self,
        cx: Request<AppState>,
        next: Next<'b, AppState>,
    ) -> BoxFuture<'c, tide::Result>
    where
        'a: 'c,
        'b: 'c,
        Self: 'c,
    {
        Box::pin(async move { Self::middleware_inner(cx, next).await })
    }
}
