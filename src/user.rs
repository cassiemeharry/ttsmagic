use anyhow::{anyhow, Context, Error, Result};
use chrono::prelude::*;
use serde::Deserialize;
use sqlx::{postgres::PgRow, Executor, Postgres, Row};
use std::{fmt, str::FromStr};
use url::Url;

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

#[derive(Clone, Debug)]
pub struct User {
    pub id: UserId,
    pub display_name: String,
    pub last_login: DateTime<Utc>,
    _other: (),
}

impl fmt::Display for User {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "User \"{}\" ({})", self.display_name, self.id)
    }
}

const DEMO_USER_ID: UserId = UserId(0);

impl User {
    async fn get_user_name_from_steam(id: u64) -> Result<String> {
        let steam_id_string = format!("{}", id);
        let url = Url::parse_with_params(
            "https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v0002/",
            &[
                ("key", crate::secrets::STEAM_API_KEY),
                ("steamids", steam_id_string.as_str()),
            ],
        )?;
        let api_request = surf::get(url);
        let mut api_response: surf::Response =
            api_request.await.map_err(Error::msg).with_context(|| {
                format!(
                    "Geting user information from Steam API for Steam login of user {}",
                    id,
                )
            })?;

        #[derive(Debug, Deserialize)]
        struct SteamAPIResponse<T> {
            response: T,
        }

        #[derive(Debug, Deserialize)]
        struct SteamAPIPlayers {
            players: Vec<SteamAPIPlayer>,
        }

        #[derive(Debug, Deserialize)]
        struct SteamAPIPlayer {
            steamid: String,
            personaname: String,
        }

        let api_response_data: SteamAPIResponse<SteamAPIPlayers> =
            api_response.body_json().await.with_context(|| {
                format!(
                    "Getting user information from Steam API response for Steam login of user {}",
                    id
                )
            })?;

        let player_info: &SteamAPIPlayer = api_response_data
            .response
            .players
            .iter()
            .filter(|p| &p.steamid == &steam_id_string)
            .take(1)
            .next()
            .ok_or_else(|| {
                anyhow!(
                    "Failed to find details for Steam user {} in {:#?}",
                    id,
                    api_response_data.response
                )
            })?;
        Ok(player_info.personaname.clone())
    }

    pub async fn steam_login(db: &mut impl Executor<Database = Postgres>, id: u64) -> Result<Self> {
        let display_name = Self::get_user_name_from_steam(id).await?;
        Self::get_or_create_user(db, id, display_name).await
    }

    pub async fn get_or_create_demo_user(
        db: &mut impl Executor<Database = Postgres>,
    ) -> Result<Self> {
        Self::get_or_create_user(db, DEMO_USER_ID.0, "Demo User".to_string()).await
    }

    pub async fn get_by_id(
        db: &mut impl Executor<Database = Postgres>,
        user_id: UserId,
    ) -> Result<Option<Self>> {
        let query = sqlx::query_as(
            "SELECT steam_id, display_name, last_login FROM ttsmagic_user WHERE steam_id = $1;",
        )
        .bind(user_id.as_queryable())
        .fetch_optional(db);
        let row_opt: Option<User> = query.await?;
        Ok(row_opt)
    }

    pub async fn get_or_create_user(
        db: &mut impl Executor<Database = Postgres>,
        steam_id: u64,
        display_name: String,
    ) -> Result<Self> {
        let id = UserId(steam_id);
        if let Some(user) = Self::get_by_id(db, id).await? {
            if &user.display_name == &display_name {
                return Ok(user);
            }
        }

        let last_login = Utc::now();
        let query = sqlx::query_as(
            "\
INSERT INTO ttsmagic_user ( steam_id, display_name, last_login ) VALUES ( $1, $2, $3 )
ON CONFLICT ( steam_id ) DO UPDATE SET display_name = $2, last_login = $3
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

// #[test]
// fn test_steam_login() {
//     const ID: u64 = 76561198026844002;

//     use async_std::task::block_on;
//     block_on(async {
//         let name = User::get_user_name_from_steam(ID).await.unwrap();
//         assert_eq!(&name, "bluejeans");
//     });
// }
