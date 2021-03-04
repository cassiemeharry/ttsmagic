//! Coordinate renders within a single process to ensure that only one runs at a
//! time (to limit memory usage).

use anyhow::Result;
use async_std::{
    sync::{Mutex, MutexGuard},
    task::sleep,
};
use redis::AsyncCommands;
use std::{collections::HashMap, convert::TryFrom as _, num::NonZeroU16, time::Duration};
use ttsmagic_types::{server_to_frontend as s2f, DeckId, UserId};

use super::notify_user;
use crate::deck::Deck;

lazy_static::lazy_static! {
    static ref PENDING: Mutex<HashMap<DeckId, UserId>> = Mutex::new(HashMap::new());
    static ref LOCK: Mutex<()> = Mutex::new(());
}

pub async fn wait_for_lock<R: AsyncCommands>(
    redis: &mut R,
    deck: &Deck,
) -> Result<MutexGuard<'static, ()>> {
    {
        let mut pending = PENDING.lock().await;
        pending.insert(deck.id, deck.user_id);
    }

    loop {
        let guard = match LOCK.try_lock() {
            None => {
                sleep(Duration::from_secs(1)).await;
                continue;
            }
            Some(guard) => guard,
        };
        let mut pending = PENDING.lock().await;
        pending.remove(&deck.id);

        // Notify all other listeners that the queue has changed length.
        let queue_length = u16::try_from(pending.len())
            .unwrap_or(u16::MAX)
            .saturating_add(1);
        let queue_length = NonZeroU16::new(queue_length).unwrap();
        for (deck_id, user_id) in pending.iter() {
            let notification = s2f::Notification::RenderProgress {
                deck_id: *deck_id,
                progress: s2f::RenderProgress::Waiting { queue_length },
            };
            notify_user(redis, *user_id, notification).await?;
        }
        return Ok(guard);
    }
}
