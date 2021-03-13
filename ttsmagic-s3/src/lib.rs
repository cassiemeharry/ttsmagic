//! # ttsmagic-s3
//!
//! This is an S3 client that uses [Surf](https://crates.io/crates/surf). All of
//! the crates I found already published, such as
//! [Rusoto](https://crates.io/crates/rusoto_s3), required Tokio to be the
//! active async executor. `ttsmagic-server` is based on async-std, which makes
//! those other crates a pain to deal with.

#![deny(missing_docs)]
#![feature(generic_associated_types)]
#![allow(incomplete_features)]

#[macro_use]
extern crate log;

use async_std::{io::Read, prelude::*};
use futures::future::BoxFuture;
use std::{convert::TryInto, time::Duration};
use surf::{http::Error, middleware::Middleware};
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
                surf::http::bail!("Failed to set scheme in S3Region::new");
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

impl Middleware for SignedContentMiddleware {
    fn handle<'a, 'b, 'c>(
        &'a self,
        mut req: surf::Request,
        client: surf::Client,
        next: surf::middleware::Next<'b>,
    ) -> BoxFuture<'c, Result<surf::Response, Error>>
    where
        'a: 'c,
        'b: 'c,
        Self: 'c,
    {
        Box::pin(async move {
            add_aws4_signature(&self.creds, &self.region, &mut req).await?;
            let result = next.run(req, client).await;
            match result {
                Ok(resp) => {
                    trace!("Request finished, status was {:?}", resp.status());
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
    request: &mut surf::Request,
) -> Result<rusoto_signature::signature::SignedRequest, Error> {
    trace!(
        "Adding AWS4 signature for region {:?} and creds {:?}",
        region,
        creds
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
    trace!("Creating signed S3 request for {} {}", method, path);
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
    trace!("Rusoto signed request: {:?}", signed);
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
    trace!("Creating pre-signed S3 URL for {} {}", method, original_url);
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
    trace!("Rusoto pre-signed URL: {:?}", presigned_str);
    let presigned_url = presigned_str.parse().unwrap();
    presigned_url
}

async fn add_aws4_signature(
    creds: &S3Credentials,
    region: &S3Region,
    request: &mut surf::Request,
) -> Result<(), Error> {
    let signed = make_rusoto_signedrequest(creds, region, request).await?;

    for (name, values_list) in signed.headers().iter() {
        let header_name: surf::http::headers::HeaderName = name.as_str().try_into().unwrap();
        request.remove_header(&header_name);
        let mut values = Vec::with_capacity(values_list.len());
        for val_bytes in values_list.iter() {
            let hv = surf::http::headers::HeaderValue::from_bytes(val_bytes.clone())?;
            values.push(hv);
        }
        let _ = request.insert_header(header_name, &*values);
    }

    Ok(())
}

/// Interactions with S3 start here.
pub trait Client: Sized + Send + Sync + 'static {
    /// A handle that uses this client to act on a specific bucket.
    type BucketHandle<'a>: BucketHandle<'a>;

    /// Focus on a specific S3 bucket.
    fn use_bucket<'a>(&'a self, name: impl ToString) -> Self::BucketHandle<'a>;
}

/// Operations that require a bucket are accessed through this object.
pub trait BucketHandle<'h>: Sized + Send + Sync {
    /// Check whether a file exists (using a HEAD request).
    fn file_exists<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>>;

    /// Download a file.
    fn get_object<'a>(
        &'a self,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Read + Unpin + Send>>>;

    /// Upload a file.
    fn put_object<'a, F>(
        &'a self,
        key: &'a str,
        file: F,
        size_hint: Option<usize>,
    ) -> BoxFuture<'a, Result<()>>
    where
        F: Read + Send + Sync + Unpin + 'static;

    /// Generate a pre-signed URL for a given file. This URL will be valid until
    /// `live_duration` seconds have passed.
    fn presign_url(&self, key: &str, live_duration: Duration) -> Url;
}

/// The real implementation of the [`Client`] trait.
#[derive(Debug)]
pub struct S3Client {
    creds: S3Credentials,
    region: S3Region,
    client: surf::Client,
}

impl S3Client {
    /// Construct a new S3 client with the given region and credentials.
    pub fn new(region: S3Region, creds: S3Credentials) -> Self {
        let client = surf::Client::new();
        Self {
            creds,
            region,
            client,
        }
    }

    #[inline]
    fn signed_request(&self, req: impl Into<surf::RequestBuilder>) -> surf::RequestBuilder {
        req.into().middleware(SignedContentMiddleware::new(
            self.creds.clone(),
            self.region.clone(),
        ))
    }
}

impl Client for S3Client {
    type BucketHandle<'a> = S3BucketHandle<'a>;

    fn use_bucket<'a>(&'a self, name: impl ToString) -> S3BucketHandle<'a> {
        let bucket_name = name.to_string();
        trace!("Using bucket {:?}", bucket_name);
        S3BucketHandle {
            bucket_name,
            client: self,
        }
    }
}

/// The real implementation of the [`BucketHandle`] trait.
#[derive(Clone, Debug)]
pub struct S3BucketHandle<'a> {
    client: &'a S3Client,
    bucket_name: String,
}

impl<'a> S3BucketHandle<'a> {
    fn file_url(&self, mut key: &str) -> Url {
        while key.starts_with('/') {
            key = &key[1..];
        }
        let mut url = self.client.region.endpoint.clone();
        url.set_path(&format!("/{}/{}", self.bucket_name, key));
        trace!("S3 URL: {:?}", url);
        url
    }
}

impl<'a> BucketHandle<'a> for S3BucketHandle<'a> {
    #[cfg(test)]
    fn file_exists<'b>(&'b self, _key: &'b str) -> BoxFuture<'b, Result<bool>> {
        panic!("Would have sent real request in test!");
    }

    #[cfg(not(test))]
    fn file_exists<'b>(&'b self, key: &'b str) -> BoxFuture<'b, Result<bool>> {
        Box::pin(async move {
            trace!(
                "Checking if file {:?} exists in bucket {:?}",
                key,
                self.bucket_name
            );
            let url = self.file_url(key);
            let req = self.client.signed_request(self.client.client.head(url));
            match self.client.client.send(req).await {
                Ok(resp) => match resp.status() as u16 {
                    404 => Ok(false),
                    200 => Ok(true),
                    other => panic!("Got unexpected status code {:?}", other),
                },
                Err(e) if e.status() as u16 == 404 => Ok(false),
                Err(e) => Err(e),
            }
        })
    }

    #[cfg(test)]
    fn get_object<'b>(
        &'b self,
        _key: &'b str,
    ) -> BoxFuture<'b, Result<Box<dyn Read + Unpin + Send>>> {
        panic!("Would have sent real request in test!");
    }

    #[cfg(not(test))]
    fn get_object<'b>(
        &'b self,
        key: &'b str,
    ) -> BoxFuture<'b, Result<Box<dyn Read + Unpin + Send>>> {
        Box::pin(async move {
            trace!("Getting file {:?} from bucket {:?}", key, self.bucket_name);
            let url = self.file_url(key);
            let req = self.client.signed_request(self.client.client.get(url));
            let resp = req.await?;
            Ok(Box::new(resp) as Box<dyn Read + Unpin + Send>)
        })
    }

    #[cfg(test)]
    fn put_object<'b, F>(
        &'b self,
        _key: &'b str,
        _file: F,
        _size_hint: Option<usize>,
    ) -> BoxFuture<'b, Result<()>>
    where
        F: Read + Send + Sync + Unpin + 'static,
    {
        panic!("Would have sent real request in test!");
    }

    #[cfg(not(test))]
    fn put_object<'b, F>(
        &'b self,
        key: &'b str,
        file: F,
        size_hint: Option<usize>,
    ) -> BoxFuture<'b, Result<()>>
    where
        F: Read + Send + Sync + Unpin + 'static,
    {
        Box::pin(async move {
            info!("Uploading file {:?} to bucket {:?}", key, self.bucket_name);
            let url = self.file_url(key);
            let file_buffer = async_std::io::BufReader::new(file);
            let body = surf::Body::from_reader(file_buffer, size_hint);
            let req = self.client.client.put(url).body(body);
            let req = self.client.signed_request(req);
            let _resp = req.await?;
            Ok(())
        })
    }

    fn presign_url(&self, key: &str, live_duration: Duration) -> Url {
        trace!(
            "Generating a pre-signed URL for file {:?} in bucket {:?}",
            key,
            self.bucket_name
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

/// Test helpers
#[allow(missing_docs)]
#[cfg_attr(not(test), allow(unused))]
pub mod tests {
    use async_std::io::Read;
    use futures::future::BoxFuture;
    use mock_it::Mock;
    #[cfg(test)]
    use pretty_assertions::assert_eq;
    use std::{pin::Pin, sync::Arc, time::Duration};
    use url::Url;

    use super::{BucketHandle, Client, Result, S3Credentials, S3Region};

    fn init() {
        let mut builder = pretty_env_logger::formatted_builder();
        builder.is_test(true);

        if let Ok(s) = std::env::var("RUST_LOG") {
            builder.parse_filters(&s);
        }

        let _ = builder.try_init();
    }

    #[derive(Clone)]
    pub struct ClientMock {
        pub use_bucket: Mock<String, BucketHandleMock>,
    }

    impl ClientMock {
        pub fn new() -> Self {
            let use_bucket = Mock::new(BucketHandleMock::new());
            Self { use_bucket }
        }
    }

    impl super::Client for ClientMock {
        type BucketHandle<'a> = BucketHandleMock;

        fn use_bucket(&self, name: impl ToString) -> BucketHandleMock {
            self.use_bucket.called(name.to_string())
        }
    }

    #[repr(transparent)]
    pub struct Factory<T>(Arc<dyn Fn() -> T + Send + Sync>);

    impl<T> Clone for Factory<T> {
        fn clone(&self) -> Self {
            Factory(Arc::clone(&self.0))
        }
    }

    impl<T> Factory<T> {
        pub fn new<F>(factory: F) -> Self
        where
            F: Fn() -> T + Send + Sync + 'static,
        {
            Self(Arc::new(factory))
        }

        pub fn get(&self) -> T {
            (self.0)()
        }
    }

    #[derive(Clone, Eq)]
    pub struct PtrEqual<T: ?Sized>(pub Arc<T>);

    impl<T: ?Sized> std::cmp::PartialEq for PtrEqual<T> {
        fn eq(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.0, &other.0)
        }
    }

    impl<T: ?Sized> From<Arc<T>> for PtrEqual<T> {
        fn from(arc: Arc<T>) -> Self {
            Self(arc)
        }
    }

    #[derive(Clone)]
    pub struct BucketHandleMock {
        pub file_exists: Arc<Mock<String, Factory<BoxFuture<'static, Result<bool>>>>>,
        pub get_object:
            Arc<Mock<String, Factory<BoxFuture<'static, Result<Box<dyn Read + Unpin + Send>>>>>>,
        pub put_object: Arc<
            Mock<
                (
                    String,
                    PtrEqual<dyn Read + Send + Sync + Unpin + 'static>,
                    Option<usize>,
                ),
                Factory<BoxFuture<'static, Result<()>>>,
            >,
        >,
        pub presign_url: Mock<(String, Duration), Url>,
    }

    impl BucketHandleMock {
        pub fn new() -> Self {
            BucketHandleMock {
                file_exists: Arc::new(Mock::new(Factory::new(|| {
                    Box::pin(async { Ok(false) }) as BoxFuture<_>
                }))),
                get_object: Arc::new(Mock::new(Factory::new(|| {
                    Box::pin(async {
                        Err(surf::Error::from_str(
                            surf::StatusCode::NotImplemented,
                            "No mock response set for ClientMock::get_object",
                        ))
                    }) as BoxFuture<_>
                }))),
                put_object: Arc::new(Mock::new(Factory::new(|| {
                    Box::pin(async { Ok(()) }) as BoxFuture<_>
                }))),
                presign_url: Mock::new(Url::parse("https://example.com/").unwrap()),
            }
        }
    }

    impl<'h> super::BucketHandle<'h> for BucketHandleMock {
        fn file_exists<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<bool>> {
            self.file_exists.called(key.into()).get()
        }

        fn get_object<'a>(
            &'a self,
            key: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn Read + Unpin + Send>>> {
            self.get_object.called(key.into()).get()
        }

        fn put_object<'a, F>(
            &'a self,
            key: &'a str,
            file: F,
            size_hint: Option<usize>,
        ) -> BoxFuture<'a, Result<()>>
        where
            F: Read + Send + Sync + Unpin + 'static,
        {
            let file = (Arc::new(file) as Arc<dyn Read + Send + Sync + Unpin + 'static>).into();
            self.put_object.called((key.into(), file, size_hint)).get()
        }

        fn presign_url(&self, key: &str, live_duration: Duration) -> Url {
            self.presign_url.called((key.to_string(), live_duration))
        }
    }

    fn make_client() -> ClientMock {
        init();
        ClientMock::new()
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

    #[cfg(test)]
    #[test]
    fn test_file_exists_no() {
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "this/file/doesn't/exist.data";

        let client = make_client();
        let handle = BucketHandleMock::new();
        handle
            .file_exists
            .given(PATH.to_string())
            .will_return(Factory::new(|| Box::pin(async { Ok(false) }) as Pin<_>));
        client
            .use_bucket
            .given(BUCKET.to_string())
            .will_return(handle);

        let exists =
            async_std::task::block_on(
                async move { client.use_bucket(BUCKET).file_exists(PATH).await },
            )
            .unwrap();
        assert!(!exists);
    }

    #[cfg(test)]
    #[test]
    fn test_file_exists_yes() {
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "does_exist.txt";

        let client = make_client();
        let handle = BucketHandleMock::new();
        handle
            .file_exists
            .given(PATH.to_string())
            .will_return(Factory::new(|| Box::pin(async { Ok(true) }) as Pin<_>));
        client
            .use_bucket
            .given(BUCKET.to_string())
            .will_return(handle);

        let exists =
            async_std::task::block_on(
                async move { client.use_bucket(BUCKET).file_exists(PATH).await },
            )
            .unwrap();
        assert!(exists);
    }

    #[cfg(test)]
    #[test]
    fn test_get_object() {
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "dummy_file.txt";
        const CONTENT: &'static [u8] = b"This is the contents of the file.";

        let client = make_client();
        let handle = BucketHandleMock::new();
        handle
            .file_exists
            .given(PATH.to_string())
            .will_return(Factory::new(|| Box::pin(async { Ok(true) }) as Pin<_>));
        handle
            .get_object
            .given(PATH.to_string())
            .will_return(Factory::new(|| {
                Box::pin(async {
                    Ok(Box::new(async_std::io::Cursor::new(CONTENT))
                        as Box<dyn Read + Unpin + Send + 'static>)
                }) as Pin<_>
            }));
        client
            .use_bucket
            .given(BUCKET.to_string())
            .will_return(handle);

        let buf = async_std::task::block_on(async {
            use async_std::prelude::*;

            let mut resp = client.use_bucket(BUCKET).get_object(PATH).await.unwrap();
            let mut buffer = Vec::with_capacity(10_000);
            resp.read_to_end(&mut buffer).await.unwrap();
            buffer
        });
        assert_eq!(&buf, CONTENT);
    }

    #[cfg(test)]
    #[test]
    fn test_put_object() {
        const BUCKET: &'static str = "ttsmagic-card-images";
        const PATH: &'static str = "test/file.data";
        const CONTENT: &'static [u8] = b"this is some content for the test file.\n";

        let client = make_client();
        let handle = BucketHandleMock::new();
        handle
            .file_exists
            .given(PATH.to_string())
            .will_return(Factory::new(|| Box::pin(async { Ok(true) }) as Pin<_>));
        handle
            .get_object
            .given(PATH.to_string())
            .will_return(Factory::new(|| {
                Box::pin(async {
                    Ok(Box::new(async_std::io::Cursor::new(CONTENT))
                        as Box<dyn Read + Unpin + Send + 'static>)
                }) as Pin<_>
            }));
        client
            .use_bucket
            .given(BUCKET.to_string())
            .will_return(handle);

        let download_buffer = async_std::task::block_on(async move {
            use async_std::prelude::*;

            let f = async_std::io::BufReader::new(CONTENT);
            let _: () = client
                .use_bucket(BUCKET)
                .put_object(PATH, f, Some(CONTENT.len()))
                .await
                .unwrap();
            let mut buffer = Vec::with_capacity(CONTENT.len());
            let mut resp = client.use_bucket(BUCKET).get_object(PATH).await.unwrap();
            resp.read_to_end(&mut buffer).await.unwrap();
            buffer
        });

        assert_eq!(download_buffer.as_slice(), CONTENT);
    }
}
