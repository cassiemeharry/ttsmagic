#![allow(unused)]

use async_std::prelude::*;

// use async_std::{pin::Pin, prelude::*};
// use futures::future::BoxFuture;
// use sqlx::{
//     postgres::{PgArguments, PgRow},
//     Executor, Postgres,
// };
// use std::{any::Any, fmt};

// pub enum MockPostgresResponse {
//     Unit,
//     Execute(u64),
//     Single(Option<PgRow>),
//     Multiple(Vec<PgRow>),
//     Error(sqlx::Error),
// }

// impl fmt::Debug for MockPostgresResponse {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         match self {
//             Self::Unit => write!(f, "Unit"),
//             Self::Execute(n) => write!(f, "Execute({:?})", n),
//             Self::Single(Some(_row)) => write!(f, "Single(Some(_))"),
//             Self::Single(None) => write!(f, "Single(None)"),
//             Self::Multiple(rows) => write!(f, "Multiple({} rows)", rows.len()),
//             Self::Error(e) => write!(f, "Error({})", e),
//         }
//     }
// }

// struct MockPostgresQueryAndResponse {
//     query: String,
//     params: Option<PgArguments>,
//     response: sqlx::Result<MockPostgresResponse>,
// }

// pub struct MockPostgresConn {
//     expected: Vec<MockPostgresQueryAndResponse>,
// }

// impl MockPostgresConn {
//     fn run_fake_query_with_args(
//         &mut self,
//         query: &str,
//         args: PgArguments,
//     ) -> sqlx::Result<MockPostgresResponse> {
//         let expected = match self.expected.pop() {
//             Some(e) => e,
//             None => panic!(
//                 "Ran out of expected queries in MockPostgresConn for query {:?}",
//                 query
//             ),
//         };
//         if query != &expected.query {
//             panic!(
//                 "Query is incorrect! Expected {:?}, got {:?}",
//                 expected.query, query
//             );
//         }
//         // if &args != expected.params {
//         //     panic!("Query parameters are incorrect!");
//         // }
//         expected.response
//     }
// }

// impl Executor for MockPostgresConn {
//     type Database = Postgres;

//     fn send<'e, 'q>(&'e mut self, command: &'q str) -> BoxFuture<'e, sqlx::Result<()>>
//     where
//         'q: 'e,
//     {
//         todo!()
//     }

//     fn fetch<'e, 'q>(
//         &'e mut self,
//         command: &'q str,
//         args: PgArguments,
//     ) -> Pin<Box<dyn Stream<Item = sqlx::Result<PgRow>> + 'e + Send>>
//     where
//         'q: 'e,
//     {
//         todo!()
//     }

//     fn fetch_optional<'e, 'q>(
//         &'e mut self,
//         command: &'q str,
//         args: PgArguments,
//     ) -> BoxFuture<'e, sqlx::Result<Option<PgRow>>>
//     where
//         'q: 'e,
//     {
//         Box::pin(async move {
//             match self.run_fake_query_with_args(command, args)? {
//                 MockPostgresResponse::Single(opt) => Ok(opt),
//                 other => panic!("Got a bad response in fetch_optional: {:?}", other),
//             }
//         })
//     }

//     fn fetch_one<'e, 'q>(
//         &'e mut self,
//         command: &'q str,
//         args: PgArguments,
//     ) -> BoxFuture<'e, sqlx::Result<PgRow>>
//     where
//         'q: 'e,
//     {
//         todo!()
//     }

//     fn execute<'e, 'q>(
//         &'e mut self,
//         command: &'q str,
//         args: PgArguments,
//     ) -> BoxFuture<'e, sqlx::Result<u64>>
//     where
//         'q: 'e,
//     {
//         todo!()
//     }

//     fn describe<'e, 'q>(
//         &'e mut self,
//         command: &'q str,
//     ) -> BoxFuture<'e, sqlx::Result<sqlx::describe::Describe<Postgres>>>
//     where
//         'q: 'e,
//     {
//         todo!()
//     }
// }

pub(crate) async fn with_test_db<F, Fu, T>(f: F) -> T
where
    for<'db> F: FnOnce(&'db mut sqlx::PgPool) -> (Future<Output = T> + 'db),
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
    let main_db_pool = sqlx::PgPool::new(&format!("{}/{}", pool_url_base, main_db_name))
        .await
        .unwrap();
    let mut main_db_conn = main_db_pool
        .acquire()
        .await
        .expect("Failed to connect to main DB");
    sqlx::query("DROP DATABASE $1;")
        .bind(test_db_name)
        .execute(&mut main_db_conn)
        .await
        .expect("Failed to drop test DB (before test)");
    sqlx::query("CREATE DATABASE $1;")
        .bind(test_db_name)
        .execute(&mut main_db_conn)
        .await
        .expect("Failed to create test DB");

    let result = {
        let mut test_db_pool = sqlx::PgPool::new(&format!("{}/{}", pool_url_base, test_db_name))
            .await
            .unwrap();
        crate::migrations::apply_all(&mut test_db_pool)
            .await
            .expect("Failed to run migrations on test DB");
        f(&mut test_db_pool).await
    };
    sqlx::query("DROP DATABASE $1;")
        .bind(test_db_name)
        .execute(&mut main_db_conn)
        .await
        .expect("Failed to drop test DB (after test)");
    result
}
