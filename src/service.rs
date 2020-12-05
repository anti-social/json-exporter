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

use jsonpath::{Match, Step};

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::time::timeout_at;
use tokio::sync::{Mutex as AsyncMutex};

use url::Url;

use crate::prepare::PreparedConfig;
use crate::convert::ResolvedMetric;

const DEFAULT_BUF_SIZE: usize = 1 << 14; // 16Kb

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
    Timeout(#[from] tokio::time::Elapsed)
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
    timeout: Duration,
    config: PreparedConfig,
    root_metric: ResolvedMetric,
    buf_size: Arc<AsyncMutex<BufSize>>,
}

struct BufSize {
    last_sizes: [usize; 10],
    ix: usize,
    current_size: usize,
}

impl BufSize {
    pub fn new() -> Self {
        Self {
            last_sizes: [DEFAULT_BUF_SIZE; 10],
            ix: 0,
            current_size: DEFAULT_BUF_SIZE,
        }
    }

    pub fn buf_size(&self) -> usize {
        self.current_size
    }

    pub fn seen(&mut self, buf_size: usize) -> bool {
        let ix = self.ix;
        self.last_sizes[ix] = buf_size;
        if ix < self.last_sizes.len() - 1 {
            self.ix += 1;
        } else {
            self.ix = 0;
        }
        let max_size = *self.last_sizes.iter().max()
            .unwrap_or(&DEFAULT_BUF_SIZE);
        if self.current_size != max_size {
            self.current_size = max_size;
            return true;
        }
        false
    }
}

impl AppState {
    pub fn new(
        config: PreparedConfig,
        namespace: Option<String>,
        global_labels: BTreeMap<String, String>,
        client: reqwest::Client,
        base_url: Url,
        timeout: Duration,
    ) -> Self {
        let root_metric = ResolvedMetric {
            metric_type: None,
            name: namespace.unwrap_or(
                config.namespace.clone().unwrap_or_else(|| "".to_string())
            ),
            labels: global_labels,
        };
        AppState {
            base_url,
            client,
            timeout,
            config,
            root_metric,
            buf_size: Arc::new(AsyncMutex::new(BufSize::new())),
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
    // println!("Global labels: {:?}", &global_labels);

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
               <p><a href="/metrics">Metrics page</a></p>
             </body>
          </html>
        "#)
}

pub async fn metrics(data: web::Data<AppState>) -> Result<impl Responder, ProcessMetricsError> {
    let mut requests_duration = Duration::default();
    let mut json_parsing_duration = Duration::default();
    let mut processing_duration = Duration::default();

    let buf_size = {
        let buf_size = data.buf_size.lock().await;
        buf_size.buf_size()
    };

    // TODO: limit fetching concurrency
    let mut resp_futures = data.config.endpoints.iter()
        .map(|endpoint| {
            let endpoint_url = endpoint.url.clone();
            let client = data.client.clone();
            let timeout = data.timeout;
            tokio::spawn(async move {
                fetch_text_content(&client, endpoint_url, timeout).await
            })
        })
        .collect::<Vec<_>>();

    let mut buf = Vec::with_capacity(buf_size);
    {
        let mut writer = GzEncoder::new(&mut buf, Compression::default());
        for (endpoint, resp_fut) in data.config.endpoints.iter().zip(resp_futures.iter_mut()) {
            let start_requests = Instant::now();
            let text_resp = resp_fut.await??;
            requests_duration += start_requests.elapsed();

            let start_parsing = Instant::now();
            let json = serde_json::from_str(&text_resp)?;
            json_parsing_duration += start_parsing.elapsed();

            let start_processing = Instant::now();
            for (level, msg) in endpoint.process(
                &data.root_metric, &json, &mut writer
            ) {
                log::log!(level, "{}", msg);
            }
            processing_duration += start_processing.elapsed();
        }
        writer.finish()?;
    }

    log::info!(
        "Timings: requests={}ms, parsing={}ms, processing={}ms",
        requests_duration.as_millis(),
        json_parsing_duration.as_millis(),
        processing_duration.as_millis(),
    );

    let mut buf_size = data.buf_size.lock().await;
    if buf_size.seen(buf.capacity()) {
        log::info!("Set new buffer size: {}", buf_size.buf_size());
    }

    Ok(
        HttpResponse::Ok()
            .content_type("text/plain; version=0.0.4")
            .header(header::CONTENT_ENCODING, ContentEncoding::Gzip.as_str())
            .body(buf)
    )
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
