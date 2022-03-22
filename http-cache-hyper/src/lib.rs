#![forbid(unsafe_code, future_incompatible)]
#![deny(
    missing_docs,
    missing_debug_implementations,
    missing_copy_implementations,
    nonstandard_style,
    unused_qualifications,
    unused_import_braces,
    unused_extern_crates,
    trivial_casts,
    trivial_numeric_casts
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! The hyper middleware implementation for http-cache.
use anyhow::anyhow;
use std::collections::HashMap;
use std::time::SystemTime;

use http::{header::CACHE_CONTROL, request::Parts, HeaderValue, Method};
use http_cache::{CacheManager, Middleware, Result};
use http_cache_semantics::CachePolicy;
use url::Url;

use hyper::{body::Bytes, client::HttpConnector, Body, Client, Request};

pub use http_cache::{
    CacheError, CacheMode, CacheOptions, HttpCache, HttpResponse,
};

#[cfg(feature = "manager-cacache")]
#[cfg_attr(docsrs, doc(cfg(feature = "manager-cacache")))]
pub use http_cache::CACacheManager;

#[cfg(feature = "manager-moka")]
#[cfg_attr(docsrs, doc(cfg(feature = "manager-moka")))]
pub use http_cache::{MokaCache, MokaCacheBuilder, MokaManager};

/// Wrapper for [`HttpCache`]
#[derive(Debug)]
pub struct Cache<T: CacheManager + Send + Sync + 'static>(
    pub HttpCache<T>,
    pub Client<HttpConnector, Body>,
);

/// Implements ['Middleware'] for hyper
pub(crate) struct HyperMiddleware {
    pub req: Request<Bytes>,
    pub client: Client<HttpConnector, Body>,
}

#[async_trait::async_trait]
impl Middleware for HyperMiddleware {
    fn is_method_get_head(&self) -> bool {
        self.req.method() == Method::GET || self.req.method() == Method::HEAD
    }
    fn policy(&self, response: &HttpResponse) -> Result<CachePolicy> {
        Ok(CachePolicy::new(&self.parts()?, &response.parts()?))
    }
    fn policy_with_options(
        &self,
        response: &HttpResponse,
        options: CacheOptions,
    ) -> Result<CachePolicy> {
        Ok(CachePolicy::new_options(
            &self.parts()?,
            &response.parts()?,
            SystemTime::now(),
            options,
        ))
    }
    fn update_headers(&mut self, parts: Parts) -> Result<()> {
        let headers = parts.headers;
        for header in headers.iter() {
            self.req.headers_mut().insert(header.0.clone(), header.1.clone());
        }
        Ok(())
    }
    fn force_no_cache(&mut self) -> Result<()> {
        self.req
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_str("no-cache")?);
        Ok(())
    }
    fn parts(&self) -> Result<Parts> {
        let mut copied_req = Request::builder()
            .uri(self.url()?.as_str())
            .method(self.req.method())
            .version(self.req.version())
            .body(())?;
        for header in self.req.headers().iter() {
            copied_req.headers_mut().append(header.0, header.1.clone());
        }
        Ok(copied_req.into_parts().0)
    }
    fn url(&self) -> Result<Url> {
        Ok(Url::parse(self.req.uri().to_string().as_str())?)
    }
    fn method(&self) -> Result<String> {
        Ok(self.req.method().as_ref().to_string())
    }
    async fn remote_fetch(&mut self) -> Result<HttpResponse> {
        let url = self.url()?.clone();
        let mut copied_req: Request<Body> = Request::builder()
            .uri(url.as_str())
            .method(self.req.method())
            .version(self.req.version())
            .body(Body::from(self.req.body().clone()))?;
        for header in self.req.headers().iter() {
            copied_req.headers_mut().append(header.0, header.1.clone());
        }
        let res = match self.client.request(copied_req).await {
            Ok(r) => r,
            Err(e) => return Err(CacheError::General(anyhow!(e))),
        };
        let mut headers = HashMap::new();
        for header in res.headers() {
            headers.insert(
                header.0.as_str().to_owned(),
                header.1.to_str()?.to_owned(),
            );
        }
        let status = res.status().into();
        let version = res.version();
        let body: Vec<u8> =
            match hyper::body::to_bytes(res.into_body()).await {
                Ok(b) => b,
                Err(e) => return Err(CacheError::General(anyhow!(e))),
            }
            .to_vec();
        Ok(HttpResponse {
            body,
            headers,
            status,
            url,
            version: version.try_into()?,
        })
    }
}
