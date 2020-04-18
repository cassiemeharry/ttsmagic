use anyhow::{anyhow, Context, Result};
use cookie::Cookie;
use futures::future::BoxFuture;
use redis::AsyncCommands;
use sqlx::{Executor, Postgres};
use std::str::FromStr;
use tide::{Next, Request, Response};
use uuid::Uuid;

use crate::{
    user::{User, UserId},
    web::AppState,
};

#[derive(Clone, Debug)]
pub struct Session {
    pub session_id: Uuid,
    pub user: Option<User>,
}

pub const SESSION_EXPIRE_SECONDS: usize = 3 * 24 * 60 * 60;
pub const SESSION_COOKIE_NAME: &'static str = "ttsmagic-session";

impl Session {
    pub fn new(user: User) -> Self {
        Session {
            session_id: Uuid::new_v4(),
            user: Some(user),
        }
    }

    pub async fn new_from_user_id(
        db: &mut impl Executor<Database = Postgres>,
        user_id: UserId,
    ) -> Result<Self> {
        let new_session_id = Uuid::new_v4();
        Self::from_user_id(db, new_session_id, user_id).await
    }

    async fn from_user_id(
        db: &mut impl Executor<Database = Postgres>,
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

    async fn load_from_cache(
        db: &mut impl Executor<Database = Postgres>,
        redis: &mut redis::aio::Connection,
        session_id: &str,
    ) -> Result<Option<Self>> {
        let session_id = Uuid::from_str(session_id)?;
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

    fn make_cookie(&self) -> Cookie<'static> {
        Cookie::build(SESSION_COOKIE_NAME, self.session_id.to_string())
            .path("/")
            // .secure(true)
            .http_only(true)
            .max_age(chrono::Duration::days(7))
            .finish()
    }
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

impl<'a, DB: Executor<Database = Postgres> + Send, T: Send + Sync> SessionGetExt<'a>
    for (
        &'a mut DB,
        &'a mut redis::aio::Connection,
        &'a http_0_2::Request<T>,
    )
{
    fn get_session(self) -> BoxFuture<'a, Option<Session>> {
        async fn inner<T>(
            (db, redis, request): (
                &mut impl Executor<Database = Postgres>,
                &mut redis::aio::Connection,
                &http_0_2::Request<T>,
            ),
        ) -> Result<Option<Session>> {
            let headers = request.headers();
            let cookie_header = headers
                .get("Cookie")
                .ok_or_else(|| anyhow!("Cookie header is missing"))
                .context("Getting session information from http::Request")?;
            let cookie_str = cookie_header
                .to_str()
                .context("Getting session information from http::Request")?;
            let s_opt = from_cookie_header(db, redis, cookie_str).await?;
            Ok(s_opt)
        }
        Box::pin(async move {
            match inner(self).await {
                Ok(s_opt) => s_opt,
                Err(e) => {
                    error!("Error when getting a session out of a http::Request: {}", e);
                    None
                }
            }
        })
    }
}

impl<'a> SessionGetExt<'static> for &'a tide::Request<AppState> {
    fn get_session(self) -> BoxFuture<'static, Option<Session>> {
        let state = self.state().clone();
        let cookie_opt_opt = self.cookie(SESSION_COOKIE_NAME).ok();
        drop(self);
        Box::pin(async move {
            let cookie = cookie_opt_opt??;
            let mut redis_conn = match state.redis.get_async_connection().await {
                Ok(c) => c,
                Err(e) => {
                    error!(
                        "Error connecting to Redis while getting session from tide::Request: {}",
                        e
                    );
                    return None;
                }
            };
            let mut db = match state.db_pool.acquire().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!(
                        "Error connecting to DB while getting session from tide::Request: {}",
                        e
                    );
                    return None;
                }
            };
            match Session::load_from_cache(&mut db, &mut redis_conn, cookie.value()).await {
                Ok(s) => s,
                Err(e) => {
                    error!("Error getting session from tide::Request: {}", e);
                    None
                }
            }
        })
    }
}

impl<'a> SessionSetExt<'a> for &'a mut tide::Response {
    fn set_session(
        self,
        redis: &'a mut redis::aio::Connection,
        session: Session,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            session.save_to_cache(redis).await?;
            let cookie = session.make_cookie();
            self.set_cookie(cookie);
            Ok(())
        })
    }
}

pub async fn from_cookie_header(
    db: &mut impl Executor<Database = Postgres>,
    redis: &mut redis::aio::Connection,
    header_value: &str,
) -> Result<Option<Session>> {
    for cookie_str_untrimmed in header_value.split("; ") {
        let cookie_str = cookie_str_untrimmed.trim();
        if !cookie_str.starts_with(SESSION_COOKIE_NAME) {
            continue;
        }
        let cookie = cookie::Cookie::parse(cookie_str)
            .with_context(|| format!("Parsing cookie header {:?}", header_value))?;
        let session_opt = Session::load_from_cache(db, redis, cookie.value())
            .await
            .with_context(|| format!("Loading from cache based on cookie {:?}", cookie))?;
        return Ok(session_opt);
    }
    Ok(None)
}

// async fn from_cookies(raw_cookies: &[&str], redis: &redis::Client) -> Result<Option<Session>> {
// }

async fn middleware_inner<'a>(
    req: Request<AppState>,
    next: Next<'a, AppState>,
) -> Result<Response> {
    let fresh_session = Session {
        session_id: Uuid::new_v4(),
        user: None,
    };
    let session;
    {
        let cookie = req.cookie(SESSION_COOKIE_NAME);
        let state = &req.state();
        let redis_client = &state.redis;
        let mut redis_conn = redis_client.get_async_connection().await?;
        let db_pool = &state.db_pool;
        let mut db = db_pool.acquire().await?;
        match cookie {
            Ok(None) => session = fresh_session,
            Ok(Some(cookie)) => {
                match Session::load_from_cache(&mut db, &mut redis_conn, cookie.value()).await? {
                    Some(s) => session = s,
                    None => session = fresh_session,
                }
            }
            Err(e) => return Err(anyhow!("Failed to get cookie: {:?}", e)),
        };
        session.save_to_cache(&mut redis_conn).await?;
    }
    let resp = next.run(req).await;
    Ok(resp)
}

pub fn middleware<'a>(req: Request<AppState>, next: Next<'a, AppState>) -> BoxFuture<'a, Response> {
    Box::pin(async move {
        match middleware_inner(req, next).await {
            Ok(resp) => resp,
            Err(e) => {
                error!("Session middleware failed: {}", e);
                Response::new(500)
            }
        }
    })
}
