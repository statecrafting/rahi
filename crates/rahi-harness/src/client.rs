//! The HTTP client a test drives the cell with (spec 033 B-3): reqwest
//! with a [`CookieJar`] of its own, no automatic redirects (a test wants
//! to see each hop and the jar wants every `Set-Cookie`), and the
//! chassis's double-submit CSRF proof on every non-safe request.

use std::sync::Mutex;

use reqwest::header::{COOKIE, HeaderValue, SET_COOKIE};
use reqwest::{Method, Response, StatusCode};
use serde::Serialize;

use crate::{CookieJar, Error, Result};

/// The header the chassis expects the CSRF cookie echoed in (spec 020 B-4).
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The CSRF cookie names, over `https` and over plain `http`.
pub const CSRF_COOKIES: [&str; 2] = ["__Host-csrf", "csrf"];

/// One client, one jar, one origin.
#[derive(Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    https: bool,
    jar: Mutex<CookieJar>,
}

impl Client {
    /// A client for `base` (`http://127.0.0.1:<port>`) with an empty jar.
    ///
    /// # Panics
    ///
    /// Never in practice: the builder only fails when TLS cannot be set up,
    /// and this client makes plain requests to loopback.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("rahi-harness")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            https: base.starts_with("https://"),
            jar: Mutex::new(CookieJar::new()),
        }
    }

    /// The origin this client talks to.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The absolute URL of `path` (or `path` itself when absolute).
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{}{path}", self.base)
        }
    }

    /// The value of the cookie called `name`, if the jar holds it.
    #[must_use]
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.jar
            .lock()
            .ok()
            .and_then(|jar| jar.get(name).map(str::to_owned))
    }

    /// Run `f` over the jar.
    pub fn with_jar<T>(&self, f: impl FnOnce(&mut CookieJar) -> T) -> Option<T> {
        self.jar.lock().ok().map(|mut jar| f(&mut jar))
    }

    /// The CSRF token the cell has issued, fetching one first when the jar
    /// holds none: a safe request to `/` mints the cookie (spec 020 B-4).
    ///
    /// # Errors
    ///
    /// [`Error::Http`] when the cell does not answer or issues no cookie.
    pub async fn csrf(&self) -> Result<String> {
        if let Some(token) = self.csrf_from_jar() {
            return Ok(token);
        }
        self.send(self.builder(Method::GET, "/")).await?;
        self.csrf_from_jar()
            .ok_or_else(|| Error::Http("the cell issued no CSRF cookie on GET /".to_owned()))
    }

    fn csrf_from_jar(&self) -> Option<String> {
        CSRF_COOKIES.iter().find_map(|name| self.cookie(name))
    }

    /// A request builder for `method` on `path` with the jar's cookies and,
    /// for a non-safe method, the CSRF header. Send it with [`Self::send`].
    ///
    /// # Errors
    ///
    /// As [`Self::csrf`], for a non-safe method.
    pub async fn request(&self, method: Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let safe = matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
        let exempt = self.path_of(path).starts_with("/auth/");
        let mut builder = self.builder(method, path);
        if !safe && !exempt {
            builder = builder.header(CSRF_HEADER, self.csrf().await?);
        }
        Ok(builder)
    }

    /// The request path of `path`, absolute or not.
    fn path_of(&self, path: &str) -> String {
        url::Url::parse(&self.url(path))
            .map(|u| u.path().to_owned())
            .unwrap_or_else(|_| "/".to_owned())
    }

    /// A builder with the jar's cookies and nothing else.
    fn builder(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = self.url(path);
        let request_path = self.path_of(path);
        let mut builder = self.http.request(method, &url);
        if let Some(cookie) = self
            .jar
            .lock()
            .ok()
            .and_then(|jar| jar.header_for(&request_path, self.https))
        {
            builder = builder.header(COOKIE, cookie);
        }
        builder
    }

    /// Send a builder from [`Self::request`], storing every `Set-Cookie`.
    ///
    /// # Errors
    ///
    /// [`Error::Http`] when the request fails at the transport.
    pub async fn send(&self, builder: reqwest::RequestBuilder) -> Result<Response> {
        let response = builder.send().await?;
        if let Ok(mut jar) = self.jar.lock() {
            for value in response.headers().get_all(SET_COOKIE) {
                if let Ok(text) = value.to_str() {
                    jar.store(text);
                }
            }
        }
        Ok(response)
    }

    /// `GET path`.
    ///
    /// # Errors
    ///
    /// As [`Self::send`].
    pub async fn get(&self, path: &str) -> Result<Response> {
        let builder = self.request(Method::GET, path).await?;
        self.send(builder).await
    }

    /// `POST path` with a JSON body and the CSRF proof.
    ///
    /// # Errors
    ///
    /// As [`Self::send`] and [`Self::csrf`].
    pub async fn post_json<T: Serialize + ?Sized>(&self, path: &str, body: &T) -> Result<Response> {
        let builder = self.request(Method::POST, path).await?.json(body);
        self.send(builder).await
    }

    /// `POST path` with an empty body and the CSRF proof.
    ///
    /// # Errors
    ///
    /// As [`Self::send`] and [`Self::csrf`].
    pub async fn post(&self, path: &str) -> Result<Response> {
        let builder = self.request(Method::POST, path).await?;
        self.send(builder).await
    }

    /// `GET path` and fail unless the status is `expected`.
    ///
    /// # Errors
    ///
    /// [`Error::Http`] naming the status and the body otherwise.
    pub async fn expect_get(&self, path: &str, expected: StatusCode) -> Result<String> {
        let response = self.get(path).await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status == expected {
            Ok(body)
        } else {
            Err(Error::Http(format!(
                "GET {path} answered {status}, expected {expected}: {body}"
            )))
        }
    }

    /// Follow one redirect: the `Location` of `response`, resolved against
    /// this origin, fetched with the jar. `None` when it is not a redirect.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    pub async fn follow(&self, response: &Response) -> Result<Option<Response>> {
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v: &HeaderValue| v.to_str().ok())
        else {
            return Ok(None);
        };
        let target = if location.starts_with('/') {
            format!("{}{location}", self.base)
        } else {
            location.to_owned()
        };
        self.get(&target).await.map(Some)
    }
}
