#![cfg(test)]
#![allow(unused)]

use async_std::prelude::*;
use futures::future::BoxFuture;

pub fn init_logging() {
    let mut builder = pretty_env_logger::formatted_timed_builder();
    builder.is_test(true);

    if let Ok(s) = std::env::var("RUST_LOG") {
        builder.parse_filters(&s);
    }

    let _ = builder.try_init();
}

#[inline]
pub(crate) fn run_with_test_db<F, T>(f: F) -> T
where
    for<'db> F: FnOnce(&'db mut sqlx::PgPool) -> BoxFuture<'db, T>,
{
    async_std::task::block_on(with_test_db(f))
}

pub(crate) async fn with_test_db<F, T>(f: F) -> T
where
    for<'db> F: FnOnce(&'db mut sqlx::PgPool) -> BoxFuture<'db, T>,
{
    let pool_url_base = format!(
        "postgresql://{}:{}@{}:{}",
        option_env!("DB_USER").unwrap_or("ttsmagic"),
        env!("DB_PASSWORD"),
        option_env!("DB_HOST").unwrap_or("localhost"),
        option_env!("DB_PORT").unwrap_or("5432"),
    );
    let main_db_name = "ttsmagic";
    let test_db_name = "ttsmagic_test";
    let main_db_pool = sqlx::PgPool::connect(&format!("{}/{}", pool_url_base, main_db_name))
        .await
        .unwrap();
    let mut main_db_conn = main_db_pool
        .acquire()
        .await
        .expect("Failed to connect to main DB");
    sqlx::query(&format!("DROP DATABASE IF EXISTS {};", test_db_name))
        .execute(&mut main_db_conn)
        .await
        .expect("Failed to drop test DB (before test)");
    sqlx::query(&format!("CREATE DATABASE {};", test_db_name))
        .execute(&mut main_db_conn)
        .await
        .expect("Failed to create test DB");

    let result = {
        let mut test_db_pool =
            sqlx::PgPool::connect(&format!("{}/{}", pool_url_base, test_db_name))
                .await
                .unwrap();
        crate::migrations::apply_all(&mut test_db_pool)
            .await
            .expect("Failed to run migrations on test DB");
        f(&mut test_db_pool).await
    };
    result
}
