use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

#[derive(Copy, Clone, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct UserId(pub u64);

#[cfg(feature = "sqlx")]
mod sqlx_impls {
    use super::*;

    impl<DB: sqlx::Database> sqlx::Type<DB> for UserId
    where
        i64: sqlx::Type<DB>,
    {
        fn type_info() -> <DB as sqlx::Database>::TypeInfo {
            <i64 as sqlx::Type<DB>>::type_info()
        }

        fn compatible(ty: &<DB as sqlx::Database>::TypeInfo) -> bool {
            <i64 as sqlx::Type<DB>>::compatible(ty)
        }
    }

    impl<'r, DB: sqlx::Database> sqlx::Decode<'r, DB> for UserId
    where
        i64: sqlx::Decode<'r, DB>,
    {
        fn decode(
            value: <DB as sqlx::database::HasValueRef<'r>>::ValueRef,
        ) -> Result<Self, Box<dyn std::error::Error + Send + Sync + 'static>> {
            let value = <i64 as sqlx::Decode<DB>>::decode(value)?;
            Ok(value.into())
        }
    }
}

impl UserId {
    pub fn as_queryable(self) -> i64 {
        self.0 as i64
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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub struct User {
    pub id: UserId,
    pub display_name: String,
}
