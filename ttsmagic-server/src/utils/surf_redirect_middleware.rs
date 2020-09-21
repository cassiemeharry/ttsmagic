use async_std::prelude::*;
use futures::future::BoxFuture;
use surf::{
    http_types::{
        headers::{HeaderName, HeaderValue},
        Cookie, Method, Url, Version,
    },
    middleware::{HttpClient, Middleware, Next, Request, Response},
};
use thiserror::Error as ErrorDerive;

struct RequestInfo {
    method: Method,
    url: Url,
    headers: Vec<(HeaderName, Vec<HeaderValue>)>,
    version: Option<Version>,
    cookies: Vec<Cookie<'static>>,
    body: Vec<u8>,
}

impl RequestInfo {
    async fn new(mut req: Request) -> Result<Self, surf::http_types::Error> {
        let body: Vec<u8> = {
            let req_body = req.take_body();
            let mut buffer = Vec::with_capacity(req_body.len().unwrap_or(1024));
            let mut reader = req_body.into_reader();
            reader.read_to_end(&mut buffer).await?;
            buffer
        };
        Ok(Self {
            method: req.method(),
            url: req.url().clone(),
            headers: req
                .iter()
                .map(|(name_ref, values_ref)| (name_ref.clone(), values_ref.clone()))
                .collect(),
            version: req.version(),
            cookies: req
                .cookies()?
                .iter()
                .map(|c| c.clone().into_owned())
                .collect(),
            body,
        })
    }

    fn make_request(&self) -> Result<Request, surf::http_types::Error> {
        let mut req = Request::new(self.method.clone(), self.url.clone());
        for (header_name, header_values) in self.headers.iter() {
            req.insert_header(header_name.clone(), header_values.as_slice())?;
        }

        req.set_version(self.version);
        for cookie in self.cookies.iter() {
            req.set_cookie(cookie.clone());
        }
        req.set_body(self.body.clone());

        Ok(req)
    }
}

#[derive(ErrorDerive, Debug)]
enum RedirectMiddlewareError {
    #[error("Missing 'Location' header in redirect response for URL {0}")]
    MissingLocationHeader(Url),
    #[error("Bad 'Location' header URL {0}")]
    BadLocationValue(String),
}

pub struct RedirectMiddleware {
    limit: u8,
}

impl RedirectMiddleware {
    #[inline]
    pub const fn new() -> Self {
        Self::new_with_limit(5)
    }

    #[inline]
    pub const fn new_with_limit(limit: u8) -> Self {
        Self { limit }
    }
}

impl<C: HttpClient + Clone> Middleware<C> for RedirectMiddleware {
    fn handle<'a>(
        &'a self,
        request: Request,
        client: C,
        next: Next<'a, C>,
    ) -> BoxFuture<'a, Result<Response, surf::Error>> {
        let location_header = HeaderName::from_ascii(b"Location".to_vec()).unwrap();

        Box::pin(async move {
            trace!("Starting to handle outbound request in RedirectMiddleware");
            let mut request_data = RequestInfo::new(request).await?;
            let mut redirects: u8 = 0;
            while redirects < self.limit {
                let request: Request = request_data.make_request()?;
                let resp: Response = next.run(request, client.clone()).await?;

                if resp.status().is_redirection() {
                    redirects += 1;
                    match resp.status() as u16 {
                        // Three of the redirection codes change the request
                        // type to GET.
                        301 | 302 | 303 => request_data.method = Method::Get,
                        _ => (),
                    };
                    // FIXME: remove the unwraps and direct indexing in favor of
                    // panic-free versions.
                    let location_values = match resp.header(&location_header) {
                        Some(lvs) => lvs,
                        None => {
                            return Err(RedirectMiddlewareError::MissingLocationHeader(
                                request_data.url.clone(),
                            )
                            .into())
                        }
                    };
                    if location_values.len() > 1 {
                        warn!(
                            "Found multiple Location header values: {:?}",
                            location_values
                        );
                    }
                    let loc = match location_values.get(0) {
                        Some(l) => l.as_str(),
                        None => {
                            return Err(RedirectMiddlewareError::MissingLocationHeader(
                                request_data.url,
                            )
                            .into())
                        }
                    };
                    debug!(
                        "Got redirection #{} from {:?} to {:?}",
                        redirects,
                        request_data.url.as_str(),
                        loc
                    );
                    request_data.url = match loc.parse() {
                        Ok(url) => url,
                        Err(_) => {
                            return Err(
                                RedirectMiddlewareError::BadLocationValue(loc.to_owned()).into()
                            )
                        }
                    };
                } else {
                    trace!("Got non-redirection response for outbound request");
                    return Ok(resp);
                }
            }
            todo!()
        })
    }
}
