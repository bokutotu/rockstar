//! HTTP transport for GPUI images. GPUI's native Application otherwise starts
//! with a NullHttpClient. Blocking networking runs only on background workers.
use anyhow::{Result, bail};
use futures_lite::AsyncReadExt as _;
use gpui_kit::{
    BackgroundExecutor,
    http_client::{self, AsyncBody, HttpClient, RedirectPolicy, RequestTimeout},
};
use std::{future::Future, io::Read as _, pin::Pin, sync::Arc, time::Duration};

const MAX_BODY: u64 = 64 * 1024 * 1024;

pub(crate) struct MediaHttpClient {
    executor: BackgroundExecutor,
}
impl MediaHttpClient {
    pub fn new(executor: BackgroundExecutor) -> Arc<Self> {
        Arc::new(Self { executor })
    }
}
impl HttpClient for MediaHttpClient {
    fn user_agent(&self) -> Option<&http_client::http::HeaderValue> {
        None
    }
    fn proxy(&self) -> Option<&url::Url> {
        None
    }

    fn send(
        &self,
        request: http_client::Request<AsyncBody>,
    ) -> Pin<Box<dyn Future<Output = Result<http_client::Response<AsyncBody>>> + Send>> {
        let executor = self.executor.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let mut bytes = Vec::new();
            body.take(MAX_BODY + 1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64 > MAX_BODY {
                bail!("HTTP request exceeds 64 MiB");
            }
            executor
                .spawn(async move {
                    let url = url::Url::parse(&parts.uri.to_string())?;
                    if !matches!(url.scheme(), "http" | "https") {
                        bail!("Only HTTP(S) requests are supported");
                    }
                    let follow = !matches!(
                        parts.extensions.get::<RedirectPolicy>(),
                        Some(RedirectPolicy::NoFollow) | None
                    );
                    let timeout = parts
                        .extensions
                        .get::<RequestTimeout>()
                        .map_or(Duration::from_secs(15), |timeout| timeout.0);
                    let client = reqwest::blocking::Client::builder()
                        .timeout(timeout)
                        .connect_timeout(Duration::from_secs(10).min(timeout))
                        .redirect(if follow {
                            reqwest::redirect::Policy::limited(10)
                        } else {
                            reqwest::redirect::Policy::none()
                        })
                        .user_agent("Rockstar/0.1")
                        .build()?;
                    let response = client
                        .request(parts.method, url)
                        .headers(parts.headers)
                        .body(bytes)
                        .send()?;
                    if response.content_length().is_some_and(|len| len > MAX_BODY) {
                        bail!("HTTP image exceeds 64 MiB");
                    }
                    let mut builder = http_client::Response::builder().status(response.status());
                    *builder.headers_mut().unwrap() = response.headers().clone();
                    let mut bytes = Vec::new();
                    response.take(MAX_BODY + 1).read_to_end(&mut bytes)?;
                    if bytes.len() as u64 > MAX_BODY {
                        bail!("HTTP image exceeds 64 MiB");
                    }
                    Ok(builder.body(AsyncBody::from(bytes))?)
                })
                .await
        })
    }
}
