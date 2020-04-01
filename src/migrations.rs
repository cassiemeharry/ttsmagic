use failure::Error;
use sqlx::{Executor, Postgres};
use std::time::{Duration, Instant};

static MIGRATIONS: &[(&'static str, &'static str)] = &[
    ("0001_initial", include_str!("../schema/0001_initial.sql")),
    (
        "0002_create_user_and_decks",
        include_str!("../schema/0002_create_user_and_decks.sql"),
    ),
];

async fn apply_migration(
    db: &mut impl Executor<Database = Postgres>,
    label: &str,
    sql: &str,
) -> Result<Option<Duration>, sqlx::Error> {
    let row_opt = sqlx::query("SELECT 1 FROM migrations WHERE label = $1")
        .bind(label)
        .fetch_optional(db)
        .await?;
    match row_opt {
        Some(_) => Ok(None),
        None => {
            info!("Applying migration {}...", label);
            let start = Instant::now();
            for statement in sql.split("\n\n") {
                debug!("Running SQL: {}", statement);
                sqlx::query(statement).execute(db).await?;
            }
            debug!("Marking migration as complete...");
            sqlx::query("INSERT INTO migrations ( label ) VALUES ( $1 );")
                .bind(label)
                .execute(db)
                .await?;
            let end = Instant::now();
            Ok(Some(end.duration_since(start)))
        }
    }
}

pub async fn apply_all(db: &mut sqlx::PgPool) -> Result<(), Error> {
    info!("Running migrations...");
    sqlx::query(
        "\
CREATE TABLE IF NOT EXISTS migrations
( label TEXT NOT NULL UNIQUE
, applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);",
    )
    .execute(db)
    .await?;

    for (label, sql) in MIGRATIONS {
        let mut tx = db.begin().await?;
        match apply_migration(&mut tx, label, sql).await {
            Ok(Some(duration)) => {
                tx.commit().await?;
                info!("Applied migration {} in {:?}", label, duration);
            }
            Ok(None) => {
                tx.rollback().await?;
                info!("Migration {} was already applied", label);
            }
            Err(e) => {
                tx.rollback().await?;
                match &e {
                    sqlx::Error::Database(db_error) => {
                        error!(
                            "Failed to apply migration {}: {}",
                            label,
                            db_error.message()
                        );
                    }
                    _ => error!("Failed to apply migration {}!", label),
                }
                return Err(e)?;
            }
        }
    }
    info!("Finished applying migrations.");
    Ok(())
}
