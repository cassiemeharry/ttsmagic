use anyhow::{anyhow, Context, Error, Result};
use async_std::{prelude::*, sync::Mutex, task};
use chrono::prelude::*;
use image::RgbImage;
use serde::Deserialize;

use super::ScryfallId;
use crate::files::MediaFile;

lazy_static::lazy_static! {
    static ref LOCATION_HEADER: tide::http::headers::HeaderName =
        tide::http::headers::HeaderName::from_ascii(b"Location".to_vec())
        .unwrap();
    static ref USER_AGENT_HEADER: tide::http::headers::HeaderName =
        tide::http::headers::HeaderName::from_ascii(b"User-Agent".to_vec())
        .unwrap();
}

#[derive(Copy, Clone, Debug)]
pub enum ImageFormat {
    PNG,
    BorderCrop,
    ArtCrop,
    Large,
    Normal,
    Small,
}

impl ImageFormat {
    fn api_str(&self) -> &'static str {
        match self {
            Self::PNG => "png",
            Self::BorderCrop => "border_crop",
            Self::ArtCrop => "art_crop",
            Self::Large => "large",
            Self::Normal => "normal",
            Self::Small => "small",
        }
    }

    fn downgrade(self) -> Option<Self> {
        match self {
            Self::PNG => Some(Self::Large),
            Self::BorderCrop => Some(Self::PNG),
            Self::ArtCrop => None,
            Self::Large => Some(Self::Normal),
            Self::Normal => Some(Self::Small),
            Self::Small => None,
        }
    }

    fn raw(self) -> image::ImageFormat {
        match self {
            ImageFormat::PNG => image::ImageFormat::Png,
            ImageFormat::BorderCrop => image::ImageFormat::Jpeg,
            ImageFormat::ArtCrop => image::ImageFormat::Jpeg,
            ImageFormat::Large => image::ImageFormat::Jpeg,
            ImageFormat::Normal => image::ImageFormat::Jpeg,
            ImageFormat::Small => image::ImageFormat::Jpeg,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum ImageFace {
    Front,
    Back,
}

impl ImageFace {
    fn api_str(&self) -> &'static str {
        match self {
            Self::Front => "",
            Self::Back => "back",
        }
    }
}

impl Default for ImageFace {
    fn default() -> Self {
        Self::Front
    }
}

#[derive(Debug)]
pub struct ScryfallApi {
    last_query: Mutex<DateTime<Utc>>,
    client: surf::Client<http_client::isahc::IsahcClient>,
}

impl ScryfallApi {
    pub fn new() -> Self {
        ScryfallApi {
            last_query: Mutex::new(Utc::now()),
            client: surf::Client::new(),
        }
    }
}

impl ScryfallApi {
    fn card_image_rel_filename(id: ScryfallId, format: ImageFormat) -> String {
        let id_str = format!("{}", id);
        assert!(id_str.len() >= 4); // this should always be true because the IDs are UUIDs
        let mut path = String::with_capacity(50);
        path.push_str("cards/");
        path.push_str(&id_str[0..2]);
        path.push('/');
        path.push_str(&id_str[2..4]);
        path.push('/');
        let (suffix, ext) = match format {
            ImageFormat::PNG => ("png", "png"),
            ImageFormat::BorderCrop => ("border-crop", "jpg"),
            ImageFormat::ArtCrop => ("art-crop", "jpg"),
            ImageFormat::Large => ("large", "jpg"),
            ImageFormat::Normal => ("normal", "jpg"),
            ImageFormat::Small => ("small", "jpg"),
        };
        path.push_str(&id_str);
        path.push('_');
        path.push_str(suffix);
        path.push('.');
        path.push_str(ext);
        path
    }

    pub async fn get_image_by_id(
        &self,
        id: ScryfallId,
        format: ImageFormat,
        face: ImageFace,
    ) -> Result<RgbImage> {
        let mut format_opt = Some(format);
        let mut last_error = None;
        // Look for existing files first.
        for format in &[ImageFormat::PNG, ImageFormat::Large] {
            let rel_filename = Self::card_image_rel_filename(id, *format);
            if let Some(mut f) = MediaFile::open_if_exists(&rel_filename).await? {
                let mut buffer = vec![];
                f.read_to_end(&mut buffer).await?;
                match image::load_from_memory_with_format(buffer.as_slice(), format.raw()) {
                    Ok(i) => return Ok(i.to_rgb()),
                    Err(e) => {
                        error!(
                            "Error opening image at {}, deleting the file: {}",
                            rel_filename, e
                        );
                        MediaFile::delete(&rel_filename).await?;
                    }
                }
            }
        }
        'format: while let Some(format) = format_opt {
            let rel_filename = Self::card_image_rel_filename(id, format);
            if let Some(mut f) = MediaFile::open_if_exists(&rel_filename).await? {
                let mut buffer = vec![];
                f.read_to_end(&mut buffer).await?;
                match image::load_from_memory_with_format(buffer.as_slice(), format.raw()) {
                    Ok(i) => return Ok(i.to_rgb()),
                    Err(e) => {
                        error!(
                            "Error opening image at {}, deleting the file: {}",
                            rel_filename, e
                        );
                        MediaFile::delete(&rel_filename).await?;
                    }
                }
            }

            debug!("Downloading image (format: {:?}) for {}...", format, id);

            let mut url = format!(
                "https://api.scryfall.com/cards/{}?format=image&version={}&face={}",
                id,
                format.api_str(),
                face.api_str()
            );

            let mut loop_counter: usize = 0;
            let mut response = 'redirect: loop {
                loop_counter += 1;
                if loop_counter > 5 {
                    return Err(anyhow!("Redirect loop"));
                }
                debug!("Loading Scryfall URL: {}", url);
                let response = self.get(&url).await;
                let r = match response {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("Failed to load image from URL {}: {}", url, e);
                        last_error = Some(e);
                        format_opt = format.downgrade();
                        continue 'format;
                    }
                };
                match r.status() as u16 {
                    200 => break 'redirect r,
                    302 => {
                        let location = r
                            .header(&LOCATION_HEADER)
                            .and_then(|headers| headers.first())
                            .map(|h| h.as_str())
                            .ok_or_else(|| {
                                anyhow!("Missing Location header in Scryfall image redirect")
                            })?;
                        debug!("Got redirect from {} to {}", url, location);
                        url = location.to_string();
                        continue 'redirect;
                    }
                    other => {
                        warn!(
                            "Failed to load image from URL {}: got status {}",
                            url, other
                        );
                        last_error = Some(anyhow!(
                            "Got unexpected status {} while getting image",
                            other
                        ));
                        format_opt = format.downgrade();
                        continue 'format;
                    }
                }
            };
            let bytes = response.body_bytes().await?;
            let image =
                image::load_from_memory_with_format(bytes.as_slice(), format.raw())?.to_rgb();

            let mut f = MediaFile::create(&rel_filename).await.context(
                "Failed to begin saving card image file from Scryfall to storage backend",
            )?;
            f.write_all(bytes.as_slice())
                .await
                .context("Failed to write card image file")?;
            f.close().await.context(
                "Failed to finalize saving card image file from Scryfall to storage backend",
            )?;
            debug!(
                "Saved card image (format: {:?}) for {} to {}",
                format, id, rel_filename,
            );
            return Ok(image);
        }
        Err(last_error.unwrap())
    }

    pub async fn get_bulk_data(&self, file: &str) -> Result<impl async_std::io::Read> {
        const BULK_URL: &'static str = "https://api.scryfall.com/bulk-data";

        let mut response = self.get(BULK_URL).await?;

        #[derive(Debug, Deserialize)]
        struct BulkDataListResponse<'a> {
            object: &'a str,
            has_more: bool,
            data: Vec<BulkDataListItem<'a>>,
        }

        #[derive(Debug, Deserialize)]
        struct BulkDataListItem<'a> {
            #[serde(rename = "type")]
            type_: &'a str,
            download_uri: &'a str,
        }

        let bulk_response = response.body_bytes().await?;
        let bulk_response: BulkDataListResponse = serde_json::from_slice(bulk_response.as_slice())?;
        debug!(
            "Got bulk downloads listing with {} items",
            bulk_response.data.len()
        );
        for item in bulk_response.data {
            debug!("Looking at listing file {}", item.type_);
            if item.type_ == file {
                info!("Downloading bulk file {}", item.download_uri);
                let response = self.get(item.download_uri).await?;
                return Ok(response);
            }
        }
        Err(anyhow!("Didn't find file {} among bulk downloads", file))
    }

    async fn get(&self, url: &str) -> Result<surf::Response> {
        self.delay().await;
        let request = self.client.get(url).set_header(
            (&*USER_AGENT_HEADER).clone(),
            concat!("ttsmagic.cards/", env!("CARGO_PKG_VERSION")),
        );
        Ok(request.await.map_err(Error::msg)?)
    }

    async fn delay(&self) {
        let mut last_query = self.last_query.lock().await;
        let now = Utc::now();
        let threshold = *last_query + chrono::Duration::milliseconds(200);
        if now < threshold {
            let delta = threshold - now;
            debug!("Delaying next Scryfall request by {}", delta);
            task::sleep(delta.to_std().unwrap()).await;
        };
        *last_query = Utc::now();
    }
}
