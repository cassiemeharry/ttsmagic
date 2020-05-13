use anyhow::{anyhow, Context as _, Result};
use async_std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    pin::Pin,
    prelude::*,
    sync::{self, Receiver, Sender},
    task::{spawn, spawn_blocking, Context, JoinHandle, Poll},
};
use futures::future::BoxFuture;
use http_0_2::StatusCode;
use rusoto_core::{ByteStream, Region, RusotoError};
use rusoto_credential::ProvideAwsCredentials;
use rusoto_s3::{
    util::{PreSignedRequest, PreSignedRequestOption},
    GetObjectError, GetObjectRequest, HeadObjectError, HeadObjectRequest, PutObjectRequest,
    S3Client, S3 as _,
};
use rust_embed::RustEmbed;
use std::{collections::VecDeque, convert::TryInto, fmt};
use tokio::io::AsyncReadExt;
use url::Url;

use crate::utils::adapt_tokio_future;

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
    static ref REGION: Region = Region::Custom {
        name: "us-east-1".to_owned(),
        endpoint: "https://us-east-1.linodeobjects.com".to_owned(),
    };
}

fn make_s3_client() -> S3Client {
    S3Client::new_with(
        rusoto_core::request::HttpClient::new().unwrap(),
        crate::secrets::linode_credentials(),
        REGION.clone(),
    )
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

    // pub async fn path_exists<P: AsRef<Path>, R: AsRef<Path>>(root: R, name: P) -> Option<PathBuf> {
    //     let rel_path = name.as_ref().to_owned();
    //     let root = root.as_ref().join(FILES_FOLDER_RELATIVE);
    //     let full_filename = root.join(&rel_path);
    //     debug!(
    //         "Looking for existence of path {} (from rel path {})",
    //         full_filename.to_string_lossy(),
    //         rel_path.to_string_lossy()
    //     );
    //     if full_filename.is_file().await {
    //         Some(full_filename)
    //     } else {
    //         None
    //     }
    // }

    async fn try_get_file(
        s3_client: S3Client,
        bucket: FileBucket,
        name: String,
    ) -> Result<Vec<u8>> {
        let req = GetObjectRequest {
            bucket: bucket.to_string(),
            key: name.to_string(),
            ..Default::default()
        };
        let resp = s3_client.get_object(req).await?;
        let streaming_body = resp.body.ok_or_else(|| {
            anyhow!(
                "No streaming body found for succesful file response for path {}",
                name
            )
        })?;

        let body_size_guess: usize = resp
            .content_length
            .and_then(|x| x.try_into().ok())
            .unwrap_or(10_000);
        let mut sync_reader = streaming_body.into_async_read();
        let mut body = Vec::with_capacity(body_size_guess);
        debug!("Reading file body synchronously");
        sync_reader.read_to_end(&mut body).await?;
        Ok(body)
    }

    pub async fn open_if_exists(name: &str) -> Result<Option<fs::File>> {
        let s3_client = make_s3_client();
        let bucket = match FileBucket::for_key(name) {
            Some(b) => b,
            None => return Ok(None),
        };
        let resp_future = Self::try_get_file(s3_client, bucket, name.to_string());
        let body = match adapt_tokio_future(resp_future).await {
            Ok(r) => r,
            Err(e) => match e.downcast_ref::<RusotoError<GetObjectError>>() {
                Some(RusotoError::Service(GetObjectError::NoSuchKey(_))) => return Ok(None),
                _ => return Err(e),
            },
        };
        let mut f = spawn_blocking(move || tempfile::tempfile().map(fs::File::from)).await?;
        f.write_all(body.as_slice()).await?;
        f.flush().await?;
        f.seek(io::SeekFrom::Start(0)).await?;
        Ok(Some(f))
    }

    pub async fn get_internal_url(name: &str) -> Result<Option<Url>> {
        let bucket = match FileBucket::for_key(name) {
            Some(b) => b,
            None => return Ok(None),
        };
        let req = GetObjectRequest {
            bucket: bucket.to_string(),
            key: name.to_string(),
            ..Default::default()
        };
        let presign_opts = PreSignedRequestOption {
            expires_in: std::time::Duration::from_secs(30),
        };
        let region = &*REGION;
        let creds = crate::secrets::linode_credentials().credentials().await?;
        let raw_url = req.get_presigned_url(region, &creds, &presign_opts);
        let presigned_url = Url::parse(&raw_url)?;
        Ok(Some(presigned_url))
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
        let (temp_file, temp_path) =
            spawn_blocking(|| tempfile::NamedTempFile::new().map(|ntf| ntf.into_parts())).await?;
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

    async fn upload_file_tokio(
        s3_client: S3Client,
        bucket: FileBucket,
        name: String,
        mut file: fs::File,
    ) -> Result<()> {
        let _ = file.seek(io::SeekFrom::Start(0)).await?;
        let mut file_contents = Vec::new();
        let read_result_len = file.read_to_end(&mut file_contents).await;
        let read_result_bytes = read_result_len.map(|_| file_contents.into());
        let byte_stream_inner = async_std::stream::once(read_result_bytes);
        let byte_stream = ByteStream::new(byte_stream_inner);
        let req = PutObjectRequest {
            bucket: bucket.to_string(),
            key: name,
            body: Some(byte_stream),
            ..Default::default()
        };
        let _resp = s3_client.put_object(req).await?;
        Ok(())
    }

    async fn upload(self) -> Result<MediaFile> {
        let s3_client = make_s3_client();
        let resp_future = Self::upload_file_tokio(
            s3_client,
            self.media_file.bucket,
            self.media_file.key.clone(),
            self.temp_file,
        );
        adapt_tokio_future(resp_future).await?;
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

struct AsyncPool<E> {
    // receiver: Receiver<T>,
    // future_fn: Box<dyn Fn(Receiver<T>) -> BoxFuture<'static, Result<(), E>> + Send + 'static>,
    tasks: VecDeque<JoinHandle<Result<(), E>>>,
    any_finished: bool,
}

impl<E: Send + Sync + 'static> AsyncPool<E> {
    fn new<F, T: Send + 'static>(parallelism: usize, receiver: Receiver<T>, future_fn: F) -> Self
    where
        F: Fn(Receiver<T>) -> BoxFuture<'static, Result<(), E>> + 'static,
    {
        let mut tasks = VecDeque::with_capacity(parallelism);
        for _ in 0..parallelism {
            tasks.push_back(spawn(future_fn(receiver.clone())));
        }
        let any_finished = false;
        AsyncPool {
            // receiver,
            // future_fn,
            tasks,
            any_finished,
        }
    }
}

impl<E> Future for AsyncPool<E> {
    type Output = Result<(), E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        while let Some(mut handle) = self.tasks.pop_front() {
            match Pin::new(&mut handle).poll(cx) {
                Poll::Ready(Ok(())) => {
                    self.any_finished = true;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {
                    self.tasks.push_back(handle);
                    return Poll::Pending;
                }
            }
        }
        Poll::Ready(Ok(()))
    }
}

async fn upload_files_tokio(
    files: Receiver<(PathBuf, String)>,
    delete_after_upload: bool,
) -> Result<()> {
    let s3_client = make_s3_client();
    while let Some((path, key)) = files.recv().await {
        let bucket = match FileBucket::for_key(&key) {
            Some(b) => b,
            None => continue,
        };

        let head_req = HeadObjectRequest {
            bucket: bucket.to_string(),
            key: key.clone(),
            ..Default::default()
        };
        match s3_client.head_object(head_req).await {
            Ok(_) => {
                debug!("File at {}:{} already exists", bucket, key);
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
            Err(RusotoError::Service(HeadObjectError::NoSuchKey(_))) => (),
            Err(RusotoError::Unknown(cause)) if cause.status == StatusCode::NOT_FOUND => (),
            Err(RusotoError::Unknown(cause)) => {
                warn!(
                    "An unknown {} error occurred, assuming {}:{:?} doesn't exist\nHeaders: {:?}\nBody: {:?}",
                    cause.status,
                    bucket,
                    key,
                    cause.headers,
                    cause.body_as_str(),
                );
            }
            Err(e) => {
                return Err(anyhow::Error::from(e)).with_context(|| {
                    format!(
                        "Failed to check whether file in bucket {} exists at key {:?}",
                        bucket, key
                    )
                })
            }
        }
        let mut file = fs::File::open(&path).await.with_context(|| {
            format!(
                "Failed to open file {:?} for upload",
                path.to_string_lossy()
            )
        })?;
        let mut file_contents = Vec::new();
        let read_result_len = file.read_to_end(&mut file_contents).await;
        drop(file);
        if let Ok(l) = read_result_len.as_ref() {
            assert_eq!(*l, file_contents.len());
        }
        let read_result_bytes = read_result_len.map(|_| file_contents.into());
        let byte_stream_inner = async_std::stream::once(read_result_bytes);
        let byte_stream = ByteStream::new(byte_stream_inner);
        debug!("Uploading {:?} to bucket {}", key, bucket);
        let put_req = PutObjectRequest {
            bucket: bucket.to_string(),
            key: key.clone(),
            body: Some(byte_stream),
            ..Default::default()
        };
        let _resp = s3_client.put_object(put_req).await.with_context(|| {
            format!(
                "Failed to upload file for key {:?} to bucket {}",
                key, bucket
            )
        })?;
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
    let upload_future_fn = Box::new(move |recv| -> BoxFuture<Result<()>> {
        let upload_future = upload_files_tokio(recv, delete_after_upload);
        let adapted_future = adapt_tokio_future(upload_future);
        Box::pin(adapted_future)
    });
    let upload_pool = AsyncPool::new(
        10,
        files_receiver,
        upload_future_fn as Box<dyn Fn(Receiver<_>) -> BoxFuture<'static, _> + Send + 'static>,
    );
    let upload_handle = spawn(upload_pool);

    let cards_scanner = scan_folder(root.clone(), root.join("cards"), files_sender.clone());
    let page_scanner = scan_folder(root.clone(), root.join("page"), files_sender.clone());
    let pages_scanner = scan_folder(root.clone(), root.join("pages"), files_sender.clone());
    let joined = spawn(cards_scanner.try_join(page_scanner).try_join(pages_scanner));
    drop(files_sender);

    debug!("Waiting on upload task");
    upload_handle.await.context("Upload task failed")?;
    debug!("Upload task finished, waiting on scanner tasks");
    let ((cards_uploaded, page_uploaded), pages_uploaded) =
        joined.await.context("Scan task failed")?;
    let files_uploaded = cards_uploaded + page_uploaded + pages_uploaded;
    debug!("Scanner tasks finished, uploaded {} files", files_uploaded);
    Ok(files_uploaded)
}
