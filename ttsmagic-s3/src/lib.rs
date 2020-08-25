//! # ttsmagic-s3
//!
//! This is an S3 client that uses [Surf](https://crates.io/crates/surf). All of
//! the crates I found already published, such as
//! [Rusoto](https://crates.io/crates/rusoto_s3), required Tokio to be the
//! active async executor. `ttsmagic-server` is based on async-std, which makes
//! those other crates a pain to deal with.

#![deny(missing_docs)]

#[macro_use]
extern crate log;

use async_std::{io::Read, prelude::*};
use futures::future::BoxFuture;
use http_client::isahc::IsahcClient;
use http_types::Error;
use std::{convert::TryInto, time::Duration};
use surf::middleware::HttpClient;
use url::Url;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Stores credentials for accessing private S3 resources.
#[derive(Clone, Debug)]
pub struct S3Credentials {
    /// S3 Access Key
    pub access_key: String,
    /// S3 Secret Key
    pub secret_key: String,
}

impl S3Credentials {
    /// Bundle up S3 credentials into a single object.
    pub fn new(access_key: impl ToString, secret_key: impl ToString) -> Self {
        Self {
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
        }
    }
}

/// An S3 Region.
#[derive(Clone, Debug)]
pub struct S3Region {
    name: String,
    endpoint: Url,
}

impl S3Region {
    /// Create a S3 region from a name and an endpoint URL.
    pub fn new(name: impl ToString, endpoint: &str) -> Result<Self> {
        let mut endpoint: Url = endpoint.parse()?;
        if !endpoint.scheme().starts_with("http") {
            if let Err(()) = endpoint.set_scheme("https") {
                http_types::bail!("Failed to set scheme in S3Region::new");
            }
        }
        Ok(Self {
            name: name.to_string(),
            endpoint,
        })
    }
}

#[derive(Debug)]
struct SignedContentMiddleware {
    creds: S3Credentials,
    region: S3Region,
}

impl SignedContentMiddleware {
    fn new(creds: S3Credentials, region: S3Region) -> Self {
        Self { creds, region }
    }
}

impl<C: HttpClient> surf::middleware::Middleware<C> for SignedContentMiddleware {
    fn handle<'a>(
        &'a self,
        mut req: surf::middleware::Request,
        client: C,
        next: surf::middleware::Next<'a, C>,
    ) -> BoxFuture<'a, Result<surf::middleware::Response, Error>> {
        Box::pin(async move {
            add_aws4_signature(&self.creds, &self.region, &mut req).await?;
            let result = next.run(req, client).await;
            match result {
                Ok(resp) => {
                    debug!("Request finished, status was {:?}", resp.status());
                    Ok(resp)
                }
                Err(e) => {
                    warn!("Request failed! Error was {}", e);
                    Err(e)
                }
            }
        })
    }
}

async fn make_rusoto_signedrequest(
    creds: &S3Credentials,
    region: &S3Region,
    request: &mut surf::middleware::Request,
) -> Result<rusoto_signature::signature::SignedRequest, Error> {
    debug!(
        "Adding AWS4 signature for region {:?} and creds {:?}",
        region, creds
    );

    let original_url = request.url();
    let method = request.method().to_string();
    let path = original_url.path();
    let mut path_slice = path;
    while path_slice.starts_with('/') {
        path_slice = &path_slice[1..];
    }
    let path = path_slice;
    let region = rusoto_signature::region::Region::Custom {
        name: region.name.clone(),
        endpoint: region.endpoint.to_string(),
    };
    debug!("Creating signed S3 request for {} {}", method, path);
    let mut signed = rusoto_signature::signature::SignedRequest::new(&method, "s3", &region, path);
    signed.scheme = Some(original_url.scheme().to_owned());
    signed.hostname = original_url.host_str().map(str::to_owned);
    signed.canonical_uri = signed.canonical_path();

    // We can't read the request body non-destructively, so we "take" the body,
    // read it into a buffer, and then set it to both the rusoto request and
    // back to the original request.
    {
        let mut body = request.take_body();
        let mut bytes = Vec::with_capacity(body.len().unwrap_or(0));
        body.read_to_end(&mut bytes).await?;
        if !bytes.is_empty() {
            signed.set_payload(Some(bytes.clone()));
            request.set_body(bytes);
        }
    }

    for (hn, hvs) in request.iter() {
        let header_name = hn.as_str();
        for hv in hvs.iter() {
            signed.add_header(header_name, hv.as_str());
        }
    }

    let rusoto_creds = rusoto_signature::credential::AwsCredentials::new(
        creds.access_key.clone(),
        creds.secret_key.clone(),
        None,
        None,
    );

    signed.sign(&rusoto_creds);
    debug!("Rusoto signed request: {:?}", signed);
    Ok(signed)
}

fn generate_presigned_url(
    creds: &S3Credentials,
    region: &S3Region,
    method: &str,
    original_url: &Url,
    live_duration: Duration,
) -> Url {
    let path = original_url.path();
    let mut path_slice = path;
    while path_slice.starts_with('/') {
        path_slice = &path_slice[1..];
    }
    let path = path_slice;
    let region = rusoto_signature::region::Region::Custom {
        name: region.name.clone(),
        endpoint: region.endpoint.to_string(),
    };
    debug!("Creating pre-signed S3 URL for {} {}", method, original_url);
    let mut signed = rusoto_signature::signature::SignedRequest::new(&method, "s3", &region, path);
    signed.scheme = Some(original_url.scheme().to_owned());
    signed.hostname = original_url.host_str().map(str::to_owned);
    signed.canonical_uri = signed.canonical_path();

    let rusoto_creds = rusoto_signature::credential::AwsCredentials::new(
        creds.access_key.clone(),
        creds.secret_key.clone(),
        None,
        None,
    );

    let presigned_str = signed.generate_presigned_url(&rusoto_creds, &live_duration, false);
    debug!("Rusoto pre-signed URL: {:?}", presigned_str);
    let presigned_url = presigned_str.parse().unwrap();
    presigned_url
}

async fn add_aws4_signature(
    creds: &S3Credentials,
    region: &S3Region,
    request: &mut surf::middleware::Request,
) -> Result<(), Error> {
    let signed = make_rusoto_signedrequest(creds, region, request).await?;

    for (name, values_list) in signed.headers().iter() {
        let header_name = name.as_str().try_into().unwrap();
        request.remove_header(&header_name);
        let mut values = Vec::with_capacity(values_list.len());
        for val_bytes in values_list.iter() {
            let hv = http_types::headers::HeaderValue::from_ascii(val_bytes)?;
            values.push(hv);
        }
        request.insert_header(header_name, &*values)?;
    }

    Ok(())
}

#[repr(transparent)]
#[derive(Debug)]
struct ClientWrapper {
    inner: surf::Client<IsahcClient>,
}

impl ClientWrapper {
    fn get(&self, url: http_types::Url) -> surf::Request<impl HttpClient> {
        self.inner.get(url)
    }

    fn head(&self, url: http_types::Url) -> surf::Request<impl HttpClient> {
        self.inner.head(url)
    }

    fn put(&self, url: http_types::Url) -> surf::Request<impl HttpClient> {
        self.inner.put(url)
    }
}

/// Interactions with S3 start here.
#[derive(Debug)]
pub struct S3Client {
    creds: S3Credentials,
    region: S3Region,
    client: ClientWrapper,
}

impl S3Client {
    fn new_isahc(region: S3Region, creds: S3Credentials) -> Self {
        use isahc::{config::Configurable, HttpClient as IsahcClientBase};
        let inner = IsahcClientBase::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        let middle = IsahcClient::from_client(inner);
        let client = surf::Client::with_client(middle);
        Self {
            creds,
            region,
            client: ClientWrapper { inner: client },
        }
    }
}

impl S3Client {
    /// Initialize the S3 client with a region and a set of credentials.
    pub fn new(region: S3Region, creds: S3Credentials) -> Self {
        S3Client::new_isahc(region, creds)
    }
}

impl S3Client {
    /// Focus on a specific S3 bucket.
    pub fn use_bucket(&self, name: impl ToString) -> S3BucketHandle {
        let bucket_name = name.to_string();
        debug!("Using bucket {:?}", bucket_name);
        S3BucketHandle {
            bucket_name,
            client: self,
        }
    }

    #[inline]
    fn signed_request<C: HttpClient>(&self, req: surf::Request<C>) -> surf::Request<C> {
        req.middleware(SignedContentMiddleware::new(
            self.creds.clone(),
            self.region.clone(),
        ))
    }
}

/// Operations that require a bucket are accessed through this object.
#[derive(Clone, Debug)]
pub struct S3BucketHandle<'a> {
    client: &'a S3Client,
    bucket_name: String,
}

impl S3BucketHandle<'_> {
    fn file_url(&self, mut key: &str) -> Url {
        while key.starts_with('/') {
            key = &key[1..];
        }
        let mut url = self.client.region.endpoint.clone();
        url.set_path(&format!("/{}/{}", self.bucket_name, key));
        debug!("S3 URL: {:?}", url);
        url
    }

    /// Check whether a file exists (using a HEAD request).
    pub async fn file_exists(&self, key: &str) -> Result<bool> {
        debug!(
            "Checking if file {:?} exists in bucket {:?}",
            key, self.bucket_name
        );
        let url = self.file_url(key);
        let req = self.client.signed_request(self.client.client.head(url));
        match req.await {
            Ok(resp) => match resp.status() as u16 {
                404 => Ok(false),
                200 => Ok(true),
                other => panic!("Got unexpected status code {:?}", other),
            },
            Err(e) if e.status() as u16 == 404 => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Download a file.
    pub async fn get_object(&self, key: &str) -> Result<impl Read> {
        debug!("Getting file {:?} from bucket {:?}", key, self.bucket_name);
        let url = self.file_url(key);
        let req = self.client.signed_request(self.client.client.get(url));
        let resp = req.await?;
        Ok(resp)
    }

    /// Upload a file.
    pub async fn put_object<F>(&self, key: &str, file: F) -> Result<()>
    where
        F: Read + Send + Sync + Unpin + 'static,
    {
        info!("Uploading file {:?} to bucket {:?}", key, self.bucket_name);
        let url = self.file_url(key);
        let file_buffer = async_std::io::BufReader::new(file);
        let req = self.client.client.put(url).body(file_buffer);
        let req = self.client.signed_request(req);
        let _resp = req.await?;
        Ok(())
    }

    /// Generate a pre-signed URL for a given file. This URL will be valid until
    /// `live_duration` seconds have passed.
    pub fn presign_url(&self, key: &str, live_duration: Duration) -> Url {
        debug!(
            "Generating a pre-signed URL for file {:?} in bucket {:?}",
            key, self.bucket_name
        );
        let url = self.file_url(key);
        generate_presigned_url(
            &self.client.creds,
            &self.client.region,
            "GET",
            &url,
            live_duration,
        )
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    fn init() {
        let mut builder = pretty_env_logger::formatted_builder();
        builder.is_test(true);

        if let Ok(s) = std::env::var("RUST_LOG") {
            builder.parse_filters(&s);
        }

        let _ = builder.try_init();
    }

    fn make_client() -> super::S3Client {
        init();
        let creds = super::S3Credentials {
            access_key: env!("S3_ACCESS_KEY_ID").to_owned(),
            secret_key: env!("S3_SECRET_KEY_ID").to_owned(),
        };
        let region = super::S3Region::new(
            "us-east-1".to_owned(),
            "https://us-east-1.linodeobjects.com",
        )
        .unwrap();
        super::S3Client::new(region, creds)
    }

    trait OptionExt {
        fn unwrap_none(self);
    }

    impl<T: std::fmt::Debug> OptionExt for Option<T> {
        fn unwrap_none(self) {
            match self {
                Some(x) => panic!("Expected None, found Some({:?})", x),
                None => (),
            }
        }
    }

    #[test]
    fn test_file_exists_no() {
        let client = make_client();
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "this/file/doesn't/exist.data";
        let exists =
            async_std::task::block_on(
                async move { client.use_bucket(BUCKET).file_exists(PATH).await },
            )
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn test_file_exists_yes() {
        let client = make_client();
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "cards/e0/b5/e0b52b9c-7278-46b4-9f3c-3a7fc0c7e526_png.png";
        let exists =
            async_std::task::block_on(
                async move { client.use_bucket(BUCKET).file_exists(PATH).await },
            )
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn test_get_object() {
        let client = make_client();
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "cards/e0/b5/e0b52b9c-7278-46b4-9f3c-3a7fc0c7e526_png.png";

        let buf = async_std::task::block_on(async {
            use async_std::prelude::*;

            let mut resp = client.use_bucket(BUCKET).get_object(PATH).await.unwrap();
            let mut buffer = Vec::with_capacity(10_000);
            resp.read_to_end(&mut buffer).await.unwrap();
            buffer
        });
        const PREFIX_MAX_SIZE: usize = 250;
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
    }

    #[test]
    fn test_put_object() {
        let client = make_client();
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "test/file.data";
        const CONTENT: &'static [u8] = b"this is some content for the test file.\n";

        let download_buffer = async_std::task::block_on(async move {
            use async_std::prelude::*;

            let f = async_std::io::BufReader::new(CONTENT);
            let _: () = client.use_bucket(BUCKET).put_object(PATH, f).await.unwrap();
            let mut buffer = Vec::with_capacity(CONTENT.len());
            let mut resp = client.use_bucket(BUCKET).get_object(PATH).await.unwrap();
            resp.read_to_end(&mut buffer).await.unwrap();
            buffer
        });

        assert_eq!(download_buffer.as_slice(), CONTENT);
    }
}
