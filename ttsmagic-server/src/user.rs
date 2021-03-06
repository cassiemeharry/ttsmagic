use anyhow::{anyhow, Context, Error, Result};
use chrono::prelude::*;
use serde::Deserialize;
use sqlx::PgConnection;
use std::fmt;
use ttsmagic_types::UserId;
use url::Url;

use crate::web::SurfErrorCompat as _;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct User {
    #[sqlx(rename = "steam_id")]
    pub id: UserId,
    pub display_name: String,
    pub last_login: DateTime<Utc>,
    #[sqlx(default)]
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
                ("key", crate::secrets::steam_api_key()),
                ("steamids", steam_id_string.clone()),
            ],
        )?;
        let api_request = surf::get(url);
        let mut api_response: surf::Response =
            api_request.await.map_err(Error::msg).with_context(|| {
                format!(
                    "Failed to get user information from Steam API for Steam login of user {}",
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

        let api_response_data: SteamAPIResponse<SteamAPIPlayers> = api_response
            .body_json()
            .await
            .surf_compat()
            .with_context(|| {
                format!(
                "Failed to get user information from Steam API response for Steam login of user {}",
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

    pub async fn steam_login(db: &mut PgConnection, id: u64) -> Result<Self> {
        let display_name = Self::get_user_name_from_steam(id).await?;
        Self::get_or_create_user(db, id, display_name).await
    }

    pub async fn get_or_create_demo_user(db: &mut PgConnection) -> Result<Self> {
        Self::get_or_create_user(db, DEMO_USER_ID.0, "Demo User".to_string()).await
    }

    pub async fn get_by_id(db: &mut PgConnection, user_id: UserId) -> Result<Option<Self>> {
        let query = sqlx::query_as(
            "SELECT steam_id, display_name, last_login FROM ttsmagic_user WHERE steam_id = $1;",
        )
        .bind(user_id.as_queryable())
        .fetch_optional(db);
        let row_opt: Option<User> = query.await?;
        Ok(row_opt)
    }

    pub async fn get_or_create_user(
        db: &mut PgConnection,
        steam_id: u64,
        display_name: String,
    ) -> Result<Self> {
        let id = UserId(steam_id);
        if let Some(user) = Self::get_by_id(&mut *db, id).await? {
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
        .fetch_one(&mut *db);
        let user = query.await?;
        Ok(user)
    }
}

// impl sqlx::FromRow<'_, PgRow> for User {
//     fn from_row(row: &PgRow) -> Result<User, sqlx::Error> {
//         let steam_id: i64 = row.try_get("steam_id")?;
//         let display_name = row.try_get("display_name")?;
//         let last_login = row.try_get("last_login")?;
//         let row = User {
//             id: UserId::from(steam_id),
//             display_name,
//             last_login,
//             _other: (),
//         };
//         Ok(row)
//     }
// }

impl<'a> Into<sentry::protocol::User> for &'a User {
    fn into(self) -> sentry::protocol::User {
        sentry::protocol::User {
            id: Some(self.id.to_string()),
            username: Some(self.display_name.clone()),
            ..Default::default()
        }
    }
}
