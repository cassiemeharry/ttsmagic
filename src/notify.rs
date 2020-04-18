use anyhow::Result;
use async_std::prelude::*;
use redis::{aio::PubSub, AsyncCommands};
use serde::Serialize;

use crate::user::UserId;

fn user_channel_name(user: UserId) -> String {
    format!("user:{}", user)
}

pub async fn notify_user<M: Serialize, R: AsyncCommands>(
    redis: &mut R,
    user: UserId,
    label: &'static str,
    msg: M,
) -> Result<()> {
    let channel_name = user_channel_name(user);

    #[derive(Serialize)]
    struct OutboundMessage<M> {
        label: &'static str,
        data: M,
    }

    let outbound = OutboundMessage { label, data: msg };
    let serialized = serde_json::to_string(&outbound)?;
    redis.publish(&channel_name, serialized).await?;
    Ok(())
}

pub async fn subscribe_user<'a>(
    redis: &'a mut PubSub,
    user: UserId,
) -> Result<impl Stream<Item = serde_json::Value> + 'a> {
    let channel_name = user_channel_name(user);
    redis.subscribe(&channel_name).await?;
    let stream = redis.on_message().filter_map(move |msg| {
        let bytes: &[u8] = msg.get_payload_bytes();
        match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(v) => Some(v),
            Err(e) => {
                error!(
                    "Failed to parse message from Redis (listening on channel {}): {}",
                    channel_name, e
                );
                None
            }
        }
    });
    Ok(stream)
}
