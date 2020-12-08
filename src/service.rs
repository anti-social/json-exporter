use actix_web::{
    http,
    web,
    HttpResponse,
    Responder,
    ResponseError,
};
use actix_web::dev::HttpResponseBuilder;
use actix_web::http::{header, ContentEncoding};

use anyhow::{Error as AnyError};

use flate2::Compression;
use flate2::write::GzEncoder;

use futures::future::try_join_all;

use futures_locks::{RwLock as AsyncRwLock};

use jsonpath::{Match, Step};

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::time::timeout_at;
use tokio::sync::Semaphore;

use url::Url;

use crate::prepare::PreparedConfig;
use crate::convert::ResolvedMetric;

const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

#[derive(thiserror::Error, Debug)]
pub enum ProcessMetricsError {
    #[error("invalid url: {0}")]
    ParseUrl(#[from] url::ParseError),
    #[error("error when sending http request: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("error when join future: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("timeout: {0}")]
    Timeout(#[from] tokio::time::Elapsed),
    #[error("cache not initialized")]
    CacheNotInitialized,
}

impl ResponseError for ProcessMetricsError {
    fn error_response(&self) -> HttpResponse {
        HttpResponseBuilder::new(self.status_code())
            .body(format!("{}", self))
    }
    fn status_code(&self) -> http::StatusCode {
        use ProcessMetricsError::*;

        match self {
            Timeout(_) => http::StatusCode::GATEWAY_TIMEOUT,
            _ => http::StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    base_url: Url,
    client: reqwest::Client,
    concurrency: u8,
    timeout: Duration,
    config: PreparedConfig,
    root_metric: ResolvedMetric,
    cache: Arc<AsyncRwLock<CachedMetrics>>,
}

impl AppState {
    pub fn new(
        config: PreparedConfig,
        root_metric: ResolvedMetric,
        client: reqwest::Client,
        base_url: Url,
        concurrency: u8,
        timeout: Duration,
        cache_expiration: Duration,
    ) -> Self {
        AppState {
            base_url,
            client,
            concurrency,
            timeout,
            config,
            root_metric,
            cache: Arc::new(AsyncRwLock::new(
                CachedMetrics::new(cache_expiration)
            )),
        }
    }
}

struct CachedMetrics {
    expiration_time: Duration,
    expired_at: Instant,
    buf: Vec<u8>,
    err: Option<ProcessMetricsError>,
}

impl CachedMetrics {
    fn new(cache_expiration: Duration) -> Self {
        Self {
            expiration_time: cache_expiration,
            expired_at: Instant::now(),
            buf: vec!(),
            err: Some(ProcessMetricsError::CacheNotInitialized),
        }
    }
    fn set_ok(&mut self) {
        self.expired_at = Instant::now() + self.expiration_time;
        self.err = None;
    }

    fn set_error(&mut self, err: ProcessMetricsError) {
        self.expired_at = Instant::now() + self.expiration_time;
        self.err = Some(err);
    }

    fn is_initialized(&self) -> bool {
        #[allow(clippy::match_like_matches_macro)]
        match &self.err {
            Some(ProcessMetricsError::CacheNotInitialized) => false,
            _ => true,
        }
    }

    fn to_response(&self) -> HttpResponse {
        match &self.err {
            None => prometheus_response(self.buf.clone()),
            Some(err) => err.error_response(),
        }
    }
}

pub async fn resolve_global_labels(
    config: &PreparedConfig, client: &reqwest::Client, timeout: Duration,
) -> Result<BTreeMap<String, String>, AnyError> {
    let mut global_labels = BTreeMap::new();
    for global_label in config.global_labels.iter() {
        let text_resp = fetch_text_content(
            &client, global_label.url.clone(), timeout
        ).await?;
        let labels_json = serde_json::from_str(&text_resp)?;
        let labels_root_match = Match {
            value: &labels_json,
            path: vec!(Step::Root),
        };
        let resolved_labels = global_label.labels.resolve(&labels_root_match)?;
        global_labels.extend(resolved_labels.into_iter());
    }

    Ok(global_labels)
}

pub async fn info() -> impl Responder {
    // TODO: Show summary about backend and endpoints
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(r#"
          <!DOCTYPE html>
          <html>
            <head>
              <meta charset="utf-8">
              <title>Json Exporter</title>
            </head>
            <body>
              <p>To the <a href="/metrics">metrics page</a></p>
            </body>
          </html>
        "#)
}

pub async fn metrics(
    state: web::Data<AppState>
) -> Result<impl Responder, ProcessMetricsError> {
    {
        let cached_metrics = state.cache.read().await;
        if cached_metrics.is_initialized() &&
            Instant::now() < cached_metrics.expired_at
        {
            return Ok(cached_metrics.to_response());
        }
    }

    let mut cached_metrics = match state.cache.try_write() {
        Ok(cached_metrics) => {
            cached_metrics
        }
        Err(()) => {
            let cached_metrics = state.cache.read().await;
            return Ok(cached_metrics.to_response());
        }
    };

    let buf = &mut cached_metrics.buf;
    buf.clear();
    log::trace!("Initial buffer capacity: {}", buf.capacity());

    match process_metrics(state, buf).await {
        Ok(()) => cached_metrics.set_ok(),
        Err(e) => cached_metrics.set_error(e),
    };

    Ok(cached_metrics.to_response())
}

fn prometheus_response(data: Vec<u8>) -> HttpResponse {
    HttpResponse::Ok()
        .content_type(PROMETHEUS_CONTENT_TYPE)
        .header(header::CONTENT_ENCODING, ContentEncoding::Gzip.as_str())
        .body(data)
}

async fn process_metrics(
    state: web::Data<AppState>, buf: &mut Vec<u8>
) -> Result<(), ProcessMetricsError> {
    let mut requests_duration = Duration::default();
    let mut json_parsing_duration = Duration::default();
    let mut processing_duration = Duration::default();

    let semaphore = Arc::new(Semaphore::new(state.concurrency as usize));
    let resp_futures = state.config.endpoints.iter()
        .map(|endpoint| {
            let endpoint_url = endpoint.url.clone();
            let client = state.client.clone();
            let timeout = state.timeout;
            let semaphore = semaphore.clone();
            async move {
                let _permit = semaphore.acquire().await;
                let start_request = Instant::now();
                let resp = fetch_text_content(&client, endpoint_url, timeout).await;
                resp.map(|r| (r, start_request.elapsed()))
            }
        })
        .collect::<Vec<_>>();

    let responses = try_join_all(resp_futures).await?;

    let mut writer = GzEncoder::new(buf, Compression::default());
    for (endpoint, (text_resp, request_duration)) in
        state.config.endpoints.iter().zip(responses.iter())
    {
        requests_duration += *request_duration;

        let start_parsing = Instant::now();
        let json = serde_json::from_str(&text_resp)?;
        json_parsing_duration += start_parsing.elapsed();

        let start_processing = Instant::now();
        for (level, msg) in endpoint.process(
            &state.root_metric, &json, &mut writer
        ) {
            log::log!(level, "{}", msg);
        }
        processing_duration += start_processing.elapsed();
    }
    writer.finish()?;

    log::info!(
        "Timings: requests_total={}ms, parsing={}ms, processing={}ms",
        requests_duration.as_millis(),
        json_parsing_duration.as_millis(),
        processing_duration.as_millis(),
    );

    Ok(())
}

async fn fetch_text_content(
    client: &reqwest::Client, url: Url, timeout: Duration
) -> Result<String, ProcessMetricsError> {

    async fn fetch(client: &reqwest::Client, url: Url) -> Result<String, reqwest::Error> {
        log::debug!("Fetching url: {}", &url);
        client.get(url).send().await?
            .text().await
    }

    Ok(
        timeout_at(tokio::time::Instant::now() + timeout, async move {
            fetch(client, url).await
        }).await??
    )
}
