use sqlx::{
    decode::{Decode, DecodeError},
    encode::Encode,
    postgres::{PgTypeInfo, Postgres},
    types::{HasSqlType, TypeInfo},
};
use std::any::{type_name, Any};

#[repr(transparent)]
#[derive(Clone, Debug)]
pub struct PgArray1D<T> {
    inner: Vec<T>,
}

impl<T> PgArray1D<T> {
    pub fn get(self) -> Vec<T> {
        self.inner
    }

    fn decode_elem(raw: &[u8]) -> Result<T, DecodeError>
    where
        T: Decode<Postgres> + Any + std::fmt::Debug,
    {
        trace!(
            "Decoding array element of type {} from raw {:?}",
            type_name::<T>(),
            raw
        );
        let val = <T as Decode<Postgres>>::decode(raw)?;
        trace!("Decode successful (got {:?})", val);
        Ok(val)
    }

    fn decode_bytes(mut raw: &[u8]) -> Result<Self, DecodeError>
    where
        T: Decode<Postgres> + Any + std::fmt::Debug,
        Postgres: HasSqlType<T>,
    {
        trace!(
            "Beginning decode of PgArray1D<{:?}>, raw = {:?}",
            type_name::<T>(),
            raw,
        );
        macro_rules! read_u32 {
            () => {{
                if raw.len() < 4 {
                    return Err(DecodeError::Message(Box::new(format!(
                        "Not enough bytes to be an array"
                    ))));
                }
                let buf: [u8; 4] = [raw[0], raw[1], raw[2], raw[3]];
                raw = &raw[4..];
                let val = u32::from_be_bytes(buf);
                trace!("Decoded u32 {:?}, advancing raw to {:?}", val, raw);
                val
            }};
        }

        let ndim = read_u32!();
        if ndim == 0 {
            if raw.len() != 8 {
                return Err(DecodeError::Message(Box::new(format!(
                    "Found what looks like an empty array (with dimension 0), but there were an unexpected number of bytes remaining to parse (got {}, expected 8)",
                    raw.len(),
                ))));
            }
            return Ok(Self { inner: vec![] });
        }
        if ndim != 1 {
            return Err(DecodeError::Message(Box::new(format!(
                "Attempted to decode a {} dimensional array as a 1D array",
                ndim
            ))));
        }
        let _ign = read_u32!();
        let oid_raw = read_u32!();
        let oid = PgTypeInfo::with_oid(oid_raw);
        let expected_oid = <Postgres as HasSqlType<T>>::type_info();
        if !expected_oid.compatible(&oid) {
            return Err(DecodeError::Message(Box::new(format!(
                "Got incorrect type ID when decoding 1D array. Expected {:?}, got {:?}",
                expected_oid, oid,
            ))));
        }
        let expected_len = read_u32!() as usize;
        let _index_1 = read_u32!();
        let size_hint = raw.len() / (expected_len.max(1));

        trace!(
            "Decoding PgArray1D<{:?}>, oid = {:?}, expected_len = {:?}, index_1 = {:?}, size_hint = {:?}, raw = {:?}",
            type_name::<T>(), oid, expected_len, _index_1, size_hint, raw,
        );

        let mut elems = Vec::with_capacity(expected_len);
        while !raw.is_empty() {
            let len = read_u32!() as usize;
            let val = if len == 0xffff_ffff {
                trace!("Found null element at index {}", elems.len());
                <T as Decode<Postgres>>::decode_null()?
            } else {
                trace!("Got element byte len {:?}", len);
                let (elem_bytes, new_raw) = raw.split_at(len);
                raw = new_raw;
                Self::decode_elem(elem_bytes)?
            };
            elems.push(val);
        }
        if expected_len != elems.len() {
            return Err(DecodeError::Message(Box::new(format!(
                "Error decoding array: expected {:?} element{}, found {:?} instead",
                expected_len,
                if expected_len == 1 { "" } else { "s" },
                elems.len(),
            ))));
        }
        Ok(PgArray1D { inner: elems })
    }
}

impl<T> HasSqlType<PgArray1D<T>> for Postgres
where
    Postgres: HasSqlType<T>,
{
    fn type_info() -> PgTypeInfo {
        todo!()
    }
}

impl<T> Decode<Postgres> for PgArray1D<T>
where
    T: Decode<Postgres> + Encode<Postgres> + Any + std::fmt::Debug,
    Postgres: HasSqlType<T>,
{
    fn decode(raw: &[u8]) -> Result<Self, DecodeError> {
        match Self::decode_bytes(raw) {
            Ok(array) => Ok(array),
            Err(e) => {
                error!(
                    "Failed to decode {}, faking an empty result: {}\nInput bytes were {:?}",
                    type_name::<PgArray1D<T>>(),
                    e,
                    raw,
                );
                Ok(PgArray1D { inner: vec![] })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::init_logging;

    #[test]
    fn test_pgarray1d_string() {
        init_logging();

        let raw_bytes = &[
            0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 25, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 1, 85, 0, 0, 0,
            1, 87,
        ];
        let decoded: Vec<String> = PgArray1D::<String>::decode(raw_bytes).unwrap().get();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].as_str(), "U");
        assert_eq!(decoded[1].as_str(), "W");
    }

    #[test]
    fn test_pgarray1d_string_empty() {
        init_logging();

        let raw_bytes = &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 25];
        let decoded: Vec<String> = PgArray1D::<String>::decode(raw_bytes).unwrap().get();
        assert_eq!(decoded.len(), 0);
    }

    #[test]
    fn test_pgarray1d_i32() {
        init_logging();

        let raw_bytes = &[
            0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 23, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 1, 0,
            0, 0, 4, 0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 4,
        ];
        let decoded: Vec<i32> = PgArray1D::<i32>::decode(raw_bytes).unwrap().get();
        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded[0], 1);
        assert_eq!(decoded[1], 2);
        assert_eq!(decoded[2], 3);
        assert_eq!(decoded[3], 4);
    }

    #[test]
    fn test_pgarray1d_i32_nulls() {
        init_logging();

        let raw_bytes = &[
            0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 23, 0, 0, 0, 4, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 1,
            255, 255, 255, 255, 0, 0, 0, 4, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 4,
        ];
        let decoded: Vec<Option<i32>> = PgArray1D::<Option<i32>>::decode(raw_bytes).unwrap().get();
        assert_eq!(decoded.len(), 4);
        assert_eq!(decoded[0], Some(1));
        assert_eq!(decoded[1], None);
        assert_eq!(decoded[2], Some(3));
        assert_eq!(decoded[3], Some(4));
    }
}
