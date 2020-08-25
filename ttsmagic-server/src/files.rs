use anyhow::{anyhow, Context as _, Result};
use async_std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    pin::Pin,
    prelude::*,
    sync::{self, Receiver, Sender},
    task::{spawn, spawn_blocking, Context, Poll},
};
use futures::future::BoxFuture;
use rust_embed::RustEmbed;
use std::fmt;
use ttsmagic_s3 as s3;
use url::Url;

use crate::{utils::AsyncPool, web::TideErrorCompat};

// const FILES_FOLDER_RELATIVE: &'static str = "files";
const FILES_URL_BASE: &'static str = "https://ttsmagic.cards/files/";
// const STATIC_URL_BASE: &'static str = "https://ttsmagic.cards/static/";

/// There are several subfolders of $root/files:
///
/// * `bulk` - created by this version of the app, contains bulk card info from Scryfall.
/// * `card_data` - created by the old app, contains bulk card info.
/// * `cards` - high resolution card images, the bulk of the disk usage.
/// * `decks` - created by the old app, contains JSON TTS decks.
/// * `page` - created by the old app, contains JPGs of TTS deck pages.
/// * `pages` - created by the new app, contains JPGs of TTS deck pages.
/// * `tokens` - high resolution card images.
///
/// Of these, we only really need to serve `page` and `pages`. The former is
/// needed to support existing decks, and the latter to support newer decks.
#[derive(Copy, Clone, Debug)]
enum FileBucket {
    CardImages,
    DeckPages,
}

impl FileBucket {
    pub fn for_key(key: &str) -> Option<Self> {
        let first_slash_pos = key.find('/')?;
        let first_folder = &key[0..first_slash_pos];
        let bucket = match first_folder {
            "cards" => Self::CardImages,
            "tokens" => Self::CardImages,
            "page" => Self::DeckPages,
            "pages" => Self::DeckPages,
            _ => {
                warn!("Tried to look up FileBucket for key {:?}", key);
                return None;
            }
        };
        Some(bucket)
    }
}

impl fmt::Display for FileBucket {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::CardImages => write!(f, "ttsmagic-card-images"),
            Self::DeckPages => write!(f, "ttsmagic-deck-page-images"),
        }
    }
}

lazy_static::lazy_static! {
    static ref REGION: s3::S3Region = s3::S3Region::new(
        "us-east-1".to_owned(),
        "https://us-east-1.linodeobjects.com",
    ).unwrap();
}

fn make_s3_client() -> s3::S3Client {
    let creds = crate::secrets::linode_credentials().into();
    let region: s3::S3Region = (&*REGION).clone();
    s3::S3Client::new(region, creds)
}

#[derive(RustEmbed)]
#[folder = "static/"]
pub struct StaticFiles;

// impl StaticFiles {
//     pub fn get_url(name: &'static str) -> Result<Url> {
//         match Self::get(name) {
//             None => Err(anyhow!("Invalid static file reference: {}", name)),
//             Some(_) => {
//                 let base = Url::parse(STATIC_URL_BASE).unwrap();
//                 Ok(base.join(name)?)
//             }
//         }
//     }
// }

#[derive(Clone, Debug)]
pub struct MediaFile {
    bucket: FileBucket,
    key: String,
}

impl MediaFile {
    pub async fn create(name: &str) -> Result<WritableMediaFile> {
        let bucket = FileBucket::for_key(name)
            .ok_or_else(|| anyhow!("No bucket available for new file {:?}", name))?;
        WritableMediaFile::new(MediaFile {
            bucket,
            key: name.to_string(),
        })
        .await
    }

    async fn try_get_file(
        s3_client: s3::S3Client,
        bucket: FileBucket,
        name: &str,
    ) -> Result<impl Read, surf::Error> {
        let bucket = s3_client.use_bucket(bucket);
        bucket.get_object(name).await
    }

    async fn file_exists(key: &str) -> Result<bool> {
        let s3_client = make_s3_client();
        let bucket = FileBucket::for_key(key)
            .ok_or_else(|| anyhow!("File {:?} does not match any bucket", key))?;
        s3_client
            .use_bucket(bucket)
            .file_exists(key)
            .await
            .tide_compat()
    }

    pub async fn open_if_exists(name: &str) -> Result<Option<fs::File>> {
        let s3_client = make_s3_client();
        let bucket = match FileBucket::for_key(name) {
            Some(b) => b,
            None => return Ok(None),
        };
        let body = match Self::try_get_file(s3_client, bucket, name).await {
            Ok(r) => r,
            Err(e) if e.status() as u16 == 404 => return Ok(None),
            Err(e) => return Err(e).tide_compat(),
        };
        let mut f = spawn_blocking(move || tempfile::tempfile().map(fs::File::from)).await?;
        async_std::io::copy(body, &mut f).await?;
        f.flush().await?;
        f.seek(io::SeekFrom::Start(0)).await?;
        Ok(Some(f))
    }

    pub async fn get_internal_url(name: &str) -> Option<Url> {
        let s3_client = make_s3_client();
        let bucket = match FileBucket::for_key(name) {
            Some(b) => b,
            None => return None,
        };
        let duration = std::time::Duration::from_secs(30);
        let presigned = s3_client.use_bucket(bucket).presign_url(name, duration);
        Some(presigned)
    }

    pub async fn delete(_name: &str) -> Result<()> {
        // TODO: delete files from S3
        Ok(())
    }

    pub fn path(&self) -> String {
        format!("{}/{}", self.bucket, self.key)
    }

    pub fn url(&self) -> Result<Url> {
        let base = Url::parse(FILES_URL_BASE).unwrap();
        Ok(base.join(&self.key)?)
    }
}

#[derive(Debug)]
pub struct WritableMediaFile {
    media_file: MediaFile,
    temp_file: fs::File,
    temp_path: tempfile::TempPath,
}

impl WritableMediaFile {
    async fn new(media_file: MediaFile) -> Result<Self> {
        // This blocks
        let suffix = media_file
            .key
            .rfind('.')
            .map(|index| media_file.key[index..].to_string());
        let (temp_file, temp_path) = spawn_blocking(move || {
            match suffix {
                Some(ext) => tempfile::Builder::new().suffix(&ext).tempfile(),
                None => tempfile::NamedTempFile::new(),
            }
            .map(|ntf| ntf.into_parts())
        })
        .await?;
        Ok(Self {
            media_file,
            temp_file: temp_file.into(),
            temp_path,
        })
    }

    pub fn path(&self) -> &std::path::Path {
        self.temp_path.as_ref()
    }

    pub async fn close(self) -> Result<()> {
        let _ = self.upload().await?;
        Ok(())
    }

    pub async fn finalize(self) -> Result<MediaFile> {
        self.upload().await
    }

    async fn upload_file_internal(
        s3_client: s3::S3Client,
        bucket: FileBucket,
        name: String,
        mut file: fs::File,
    ) -> Result<String> {
        let (prefix, ext): (&str, &str) = match name.rfind('.') {
            Some(dot_index) => (&name[0..dot_index], &name[dot_index + 1..]),
            None => (name.as_str(), ""),
        };
        let mut key = name.clone();
        const RANDOM_LEN: usize = 8;
        let mut random_chars = String::with_capacity(RANDOM_LEN);
        while MediaFile::file_exists(&key).await? {
            use rand::Rng;

            random_chars.clear();
            for _ in 0..RANDOM_LEN {
                let c = rand::thread_rng().gen_range('a' as u8, ('z' as u8) + 1) as char;
                random_chars.push(c);
            }
            key = format!("{}_{}.{}", prefix, random_chars, ext);
        }
        if &key != &name {
            warn!(
                "Saving file in bucket {} as {:?} (requested name was {:?})",
                bucket, key, name
            );
        }
        let _ = file.seek(io::SeekFrom::Start(0)).await?;

        s3_client
            .use_bucket(bucket.to_string())
            .put_object(&key, file)
            .await
            .tide_compat()?;
        Ok(key)
    }

    async fn upload(mut self) -> Result<MediaFile> {
        let s3_client = make_s3_client();
        let key = Self::upload_file_internal(
            s3_client,
            self.media_file.bucket,
            self.media_file.key,
            self.temp_file,
        )
        .await?;
        self.media_file.key = key;
        Ok(self.media_file)
    }
}

impl Write for WritableMediaFile {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.temp_file).poll_write(cx, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.temp_file).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.temp_file).poll_close(cx)
    }
}

async fn upload_files(files: Receiver<(PathBuf, String)>, delete_after_upload: bool) -> Result<()> {
    let s3_client = make_s3_client();
    while let Some((path, key)) = files.recv().await {
        let bucket = match FileBucket::for_key(&key) {
            Some(b) => b,
            None => continue,
        };
        let exists = MediaFile::file_exists(&key).await?;
        if exists {
            debug!("File with key {} already exists", key);
            if delete_after_upload {
                fs::remove_file(&path).await.with_context(|| {
                    format!(
                        "Failed to delete existing file {:?}",
                        path.to_string_lossy()
                    )
                })?;
            }
            continue;
        }
        let file = fs::File::open(&path).await.with_context(|| {
            format!(
                "Failed to open file {:?} for upload",
                path.to_string_lossy()
            )
        })?;
        s3_client
            .use_bucket(bucket.to_string())
            .put_object(&key, file)
            .await
            .tide_compat()?;
        if delete_after_upload {
            fs::remove_file(&path).await.with_context(|| {
                format!(
                    "Failed to delete uploaded file {:?}",
                    path.to_string_lossy()
                )
            })?;
            info!("Uploaded {}:{:?} and removed local file", bucket, key);
        } else {
            info!(
                "Uploaded file {}:{:?} (kept local file on disk)",
                bucket, key
            );
        }
    }
    Ok(())
}

fn scan_folder(
    root: PathBuf,
    dir: PathBuf,
    sender: Sender<(PathBuf, String)>,
) -> BoxFuture<'static, Result<u64>> {
    Box::pin(async move {
        if !dir.is_dir().await {
            warn!(
                "Tried to scan non-existent folder {:?}",
                dir.to_string_lossy()
            );
            return Ok(0);
        }

        let mut files_found = 0;
        let mut dir = fs::read_dir(&dir).await?;
        while let Some(res) = dir.next().await {
            let entry = res?;
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                files_found += scan_folder(root.clone(), entry.path(), sender.clone()).await?;
            } else if file_type.is_file() {
                let path = entry.path();
                let key = path.strip_prefix(&root)?.to_string_lossy().to_string();
                debug!("Sending {:?} to upload task", key);
                sender.send((path, key)).await;
                files_found += 1;
            } else {
                assert!(file_type.is_symlink());
                warn!("Not uploading symlink {:?}", entry.path().to_string_lossy());
            }
        }
        Ok(files_found)
    })
}

pub async fn upload_all(root: PathBuf, delete_after_upload: bool) -> Result<u64> {
    let root = root.join("files");
    info!("Uploading all files in {:?}", root.to_string_lossy());

    let (files_sender, files_receiver) = sync::channel(1);
    let upload_future_fn = move |recv| -> BoxFuture<Result<()>> {
        let upload_future = upload_files(recv, delete_after_upload);
        Box::pin(upload_future)
    };
    let upload_pool = AsyncPool::new(10, files_receiver, upload_future_fn);

    let cards_scanner = scan_folder(root.clone(), root.join("cards"), files_sender.clone());
    let page_scanner = scan_folder(root.clone(), root.join("page"), files_sender.clone());
    let pages_scanner = scan_folder(root.clone(), root.join("pages"), files_sender.clone());
    let joined = spawn(cards_scanner.try_join(page_scanner).try_join(pages_scanner));
    drop(files_sender);

    debug!("Waiting on upload task");
    upload_pool.await?;
    debug!("Upload task finished, waiting on scanner tasks");
    let ((cards_uploaded, page_uploaded), pages_uploaded) =
        joined.await.context("Scan task failed")?;
    let files_uploaded = cards_uploaded + page_uploaded + pages_uploaded;
    debug!("Scanner tasks finished, uploaded {} files", files_uploaded);
    Ok(files_uploaded)
}

#[cfg(test)]
mod tests {
    fn init() {
        let mut builder = pretty_env_logger::formatted_builder();
        builder.is_test(true);

        if let Ok(s) = std::env::var("RUST_LOG") {
            builder.parse_filters(&s);
        }

        let _ = builder.try_init();
    }

    #[test]
    fn get_example_file() {
        use super::MediaFile;
        use async_std::io::ReadExt as _;

        const PREFIX_MAX_SIZE: usize = 250;

        init();

        async_std::task::block_on(async {
            let path = "cards/b3/c2/b3c2bd44-4d75-4f61-89c0-1f1ba4d59ffa_png.png";
            let opened_res = MediaFile::open_if_exists(path).await;
            println!("File opened: {:?}", opened_res);
            let mut opened = opened_res.unwrap().unwrap();
            let mut buf = Vec::with_capacity(10_000);
            opened.read_to_end(&mut buf).await.unwrap();
            let as_bytes: &[u8] = &buf[0..buf.len().min(PREFIX_MAX_SIZE)];
            let as_str: &str;
            let prefix = match std::str::from_utf8(as_bytes) {
                Ok(s) => {
                    as_str = s;
                    &as_str as &dyn std::fmt::Debug
                }
                Err(_) => &as_bytes as &dyn std::fmt::Debug,
            };
            assert!(
                buf.len() > 10_000,
                "file was truncated, len is {:?}, prefix is bytes are {:?}",
                buf.len(),
                prefix,
            );
        })
    }
}
