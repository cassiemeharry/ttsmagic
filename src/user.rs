use chrono::prelude::*;
use failure::Error;
use sqlx::{postgres::PgRow, Executor, Postgres, Row};
use std::{fmt, str::FromStr};

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct UserId(u64);

impl UserId {
    pub fn as_queryable(self) -> i64 {
        self.0 as i64
    }
}

impl From<i64> for UserId {
    fn from(x: i64) -> UserId {
        UserId::from(x as u64)
    }
}

impl From<u64> for UserId {
    fn from(x: u64) -> UserId {
        UserId(x)
    }
}

impl FromStr for UserId {
    type Err = <u64 as FromStr>::Err;
    fn from_str(id: &str) -> Result<Self, Self::Err> {
        let inner = u64::from_str(id)?;
        Ok(UserId(inner))
    }
}

impl fmt::Debug for UserId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

pub struct User {
    pub id: UserId,
    pub display_name: String,
    pub last_login: DateTime<Utc>,
    _other: (),
}

const DEMO_USER_ID: UserId = UserId(0);

impl User {
    pub async fn get_or_create_demo_user<E>(db: &mut E) -> Result<Self, Error>
    where
        E: Executor<Database = Postgres>,
    {
        let id = DEMO_USER_ID;
        let query = sqlx::query_as(
            "SELECT steam_id, display_name, last_login FROM ttsmagic_user WHERE steam_id = $1;",
        )
        .bind(id.as_queryable())
        .fetch_optional(db);
        let row_opt: Option<User> = query.await?;
        if let Some(user) = row_opt {
            return Ok(user);
        }

        let display_name = "Demo User".to_string();
        let last_login = Utc::now();
        let query = sqlx::query_as(
            "\
INSERT INTO ttsmagic_user ( steam_id, display_name, last_login ) VALUES ( $1, $2, $3 )
RETURNING *;",
        )
        .bind(id.as_queryable())
        .bind(display_name)
        .bind(last_login)
        .fetch_one(db);
        let user = query.await?;
        Ok(user)
    }
}

impl sqlx::FromRow<PgRow> for User {
    fn from_row(row: PgRow) -> User {
        let steam_id: i64 = row.get("steam_id");
        let display_name = row.get("display_name");
        let last_login = row.get("last_login");
        User {
            id: UserId::from(steam_id),
            display_name,
            last_login,
            _other: (),
        }
    }
}
