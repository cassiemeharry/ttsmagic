use anyhow::{anyhow, Context, Result};
use async_std::{prelude::*, sync::Arc};
use sqlx::{Executor, PgPool, Postgres, Row};
use ttsmagic_types::{DeckId, UserId};
use url::Url;
use uuid::Uuid;

use crate::{scryfall::api::ScryfallApi, user::User};

async fn import_users(db: &mut impl Executor<Database = Postgres>) -> Result<()> {
    sqlx::query("\
INSERT INTO ttsmagic_user ( steam_id, display_name, last_login )
SELECT ((social_auth_usersocialauth.extra_data::jsonb) -> 'player' ->> 'steamid')::bigint as steam_id
     , ((social_auth_usersocialauth.extra_data::jsonb) -> 'player' ->> 'personaname') as display_name
     , auth_user.last_login as last_login
FROM auth_user
INNER JOIN social_auth_usersocialauth
  ON (social_auth_usersocialauth.user_id = auth_user.id)
WHERE
    social_auth_usersocialauth.provider = 'steam'
ON CONFLICT (steam_id) DO NOTHING
;")
     .execute(db).await?;
    Ok(())
}

async fn import_decks(
    scryfall_api: Arc<ScryfallApi>,
    db: &mut PgPool,
    redis: &mut impl redis::AsyncCommands,
    only_user: Option<UserId>,
) -> Result<()> {
    sqlx::query(
        "\
INSERT INTO deck ( id, user_id, title, url )
SELECT (deck_deck.uuid)::uuid AS id
     , ((social_auth_usersocialauth.extra_data::jsonb) -> 'player' ->> 'steamid')::bigint as user_id
     , deck_deck.name AS title
     , deck_deck.source_url as url
FROM deck_deck
INNER JOIN auth_user
  ON (deck_deck.created_by_id = auth_user.id)
INNER JOIN social_auth_usersocialauth
  ON (social_auth_usersocialauth.user_id = auth_user.id)
WHERE
    deck_deck.name IS NOT NULL
AND deck_deck.name <> ''
AND deck_deck.source_url IS NOT NULL
AND deck_deck.source_url <> ''
AND social_auth_usersocialauth.provider = 'steam'
ON CONFLICT DO NOTHING
;",
    )
    .execute(db)
    .await?;

    let mut decks_to_load;
    {
        let mut decks_to_load_stream = sqlx::query(
            "\
SELECT DISTINCT
  ((social_auth_usersocialauth.extra_data::jsonb) -> 'player' ->> 'steamid')::bigint as user_id
, deck_deck.source_url as url
FROM deck_deck
INNER JOIN auth_user
  ON (deck_deck.created_by_id = auth_user.id)
INNER JOIN social_auth_usersocialauth
  ON (social_auth_usersocialauth.user_id = auth_user.id)
WHERE
    deck_deck.name IS NOT NULL
AND deck_deck.name <> ''
AND deck_deck.source_url IS NOT NULL
AND deck_deck.source_url <> ''
AND social_auth_usersocialauth.provider = 'steam'
AND (SELECT COUNT(*) FROM deck_entry WHERE deck_entry.deck_id = deck_deck.uuid::uuid) = 0
ORDER BY
  ((social_auth_usersocialauth.extra_data::jsonb) -> 'player' ->> 'steamid')::bigint ASC
, deck_deck.source_url
;",
        )
        .fetch(db);
        decks_to_load = Vec::new();
        while let Some(row_result) = decks_to_load_stream.next().await {
            let row = row_result?;
            let user_id: u64 = row.get::<i64, _>("user_id") as u64;
            let user_id = UserId::from(user_id);
            let url_str: String = row.get("url");
            let url = Url::parse(&url_str)?;
            decks_to_load.push((user_id, url));
        }
    }

    for (user_id, url) in decks_to_load {
        if let Some(only_user_id) = only_user {
            if user_id != only_user_id {
                continue;
            }
        }
        let mut load_tx = db.begin().await?;
        let user: User = User::get_by_id(&mut load_tx, user_id)
            .await?
            .ok_or_else(|| anyhow!("Failed to get user with ID {}", user_id))?;
        info!("Importing {}'s deck with URL {}", user, url);
        let mut deck = crate::deck::load_deck(&mut load_tx, redis, &user, url.clone())
            .await
            .with_context(|| format!("Failed to load deck for {} at URL {}", user, url))?;
        deck.render(scryfall_api.clone(), &mut load_tx, redis)
            .await
            .with_context(|| {
                format!("Failed to render deck {} for {} URL {}", deck.id, user, url)
            })?;
        load_tx.commit().await?;
        info!(
            "Finished import deck {} ({}) for {}",
            deck.title, deck.id, user
        );

        let count_row = sqlx::query(
            "SELECT COUNT(*) AS row_count FROM deck_entry WHERE deck_entry.deck_id = $1;",
        )
        .bind(deck.id.as_uuid())
        .fetch_one(db)
        .await?;
        let count: i64 = count_row.get("row_count");
        assert!(count > 0);
    }

    Ok(())
}

pub async fn import_all(
    scryfall_api: Arc<ScryfallApi>,
    db: &mut PgPool,
    redis: &mut impl redis::AsyncCommands,
    only_user: Option<UserId>,
) -> Result<()> {
    {
        info!("Importing users from old system...");
        let mut users_tx = db.begin().await?;
        import_users(&mut users_tx).await?;
        users_tx.commit().await?;
    }

    info!("Importing decks from old system...");
    import_decks(scryfall_api, db, redis, only_user).await?;

    Ok(())
}

async fn cleanup_deck(
    db: &mut PgPool,
    deck_id: DeckId,
    user_id: UserId,
    title: String,
    url: Url,
) -> Result<()> {
    info!(
        "Cleaning up deck {} \"{}\" (user {}, URL {})",
        deck_id, title, user_id, url
    );
    let loader = match crate::deck::find_loader::<PgPool, redis::aio::Connection>(url.clone()) {
        Some(l) => l,
        None => {
            warn!(
                "Failed to find loader for deck {} at URL {} (user: {})",
                deck_id, url, user_id
            );
            return Ok(());
        }
    };
    let canon_url = loader.canonical_deck_url();
    if canon_url == url {
        // This deck is fine.
        return Ok(());
    }
    if canon_url != url {
        info!(
            "Deck {} has a bad URL:\n   previous: {}\n  canonical: {}",
            deck_id, url, canon_url
        );
        // Check to see if there's a duplicate deck with the correct URL. If
        // so, delete this one. Otherwise, fix the URL.
        let exists_opt = sqlx::query("SELECT 1 FROM deck WHERE user_id = $1 AND url = $2;")
            .bind(user_id.as_queryable())
            .bind(format!("{}", canon_url))
            .fetch_optional(db)
            .await?;
        match exists_opt {
            Some(_) => {
                info!("There's already a deck for this user with the correct URL, so deleting the incorrect one.");
                sqlx::query("DELETE FROM deck WHERE id = $1;")
                    .bind(deck_id.as_uuid())
                    .execute(db)
                    .await?
            }
            None => {
                info!("Fixing the URL");
                sqlx::query("UPDATE deck SET url = $1 WHERE id = $2;")
                    .bind(format!("{}", canon_url))
                    .bind(deck_id.as_uuid())
                    .execute(db)
                    .await?
            }
        };
    }
    Ok(())
}

pub async fn cleanup(db: &mut PgPool) -> Result<()> {
    let decks_to_check;
    {
        let mut stream = sqlx::query(
            "\
SELECT id, user_id, title, url FROM deck
ORDER BY user_id ASC, url ASC
;",
        )
        .fetch(db);
        let mut to_check = Vec::new();
        while let Some(row_result) = stream.next().await {
            let row = row_result.context("Failed to get list of decks to cleanup")?;
            let user_id: u64 = row.get::<i64, _>("user_id") as u64;
            let user_id = UserId::from(user_id);
            let url_str: String = row.get("url");
            let id: Uuid = row.get("id");
            let id = DeckId(id);
            let url = match Url::parse(&url_str) {
                Ok(u) => u,
                Err(e) => {
                    error!("Failed to parse URL {:?} for deck {}: {}", url_str, id, e);
                    continue;
                }
            };
            let title: String = row.get("title");
            to_check.push((id, user_id, title, url));
        }
        decks_to_check = to_check;
    }
    for (deck_id, user_id, title, url) in decks_to_check {
        cleanup_deck(db, deck_id, user_id, title, url)
            .await
            .with_context(|| format!("Failed to clean up deck {}", deck_id))?;
    }

    Ok(())
}
