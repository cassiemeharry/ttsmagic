use anyhow::Result;
use async_std::prelude::*;
use redis::{aio::PubSub, AsyncCommands};
use ttsmagic_types::{server_to_frontend::Notification, UserId};

fn user_channel_name(user: UserId) -> String {
    format!("user:{}", user)
}

pub async fn notify_user<R: AsyncCommands>(
    redis: &mut R,
    user: UserId,
    msg: Notification,
) -> Result<()> {
    let channel_name = user_channel_name(user);
    let serialized = serde_json::to_string(&msg)?;
    debug!("Sending notification to user {:?}: {:?}", user, msg);
    redis
        .publish::<_, _, i32>(&channel_name, serialized)
        .await?;
    Ok(())
}

pub async fn subscribe_user<'a>(
    redis: &'a mut PubSub,
    user: UserId,
) -> Result<impl Stream<Item = Result<Notification>> + 'a> {
    let channel_name = user_channel_name(user);
    redis.subscribe(&channel_name).await?;
    let stream = redis.on_message().filter_map(move |msg| {
        let bytes: &[u8] = msg.get_payload_bytes();
        let notification_result =
            serde_json::from_slice::<Notification>(bytes).map_err(anyhow::Error::from);
        Some(notification_result)
    });
    Ok(stream)
}
