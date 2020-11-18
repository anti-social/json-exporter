use actix_web::{
    http,
    middleware,
    web,
    App,
    HttpResponse,
    HttpServer,
    Responder,
    ResponseError,
};
use actix_web::dev::HttpResponseBuilder;

use anyhow::{bail, Context, Error as AnyError};

use clap::Clap;

use json_exporter::read_config;
use json_exporter::prepare::PreparedConfig;
use json_exporter::convert::ResolvedMetric;

use jsonpath::{Match, Step};

use mimalloc::MiMalloc;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::time::delay_for;
use tokio::sync::{Mutex as AsyncMutex};

use url::Url;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const DEFAULT_BUF_SIZE: usize = 1 << 14; // 16Kb

#[derive(Clap, Debug)]
struct Opts {
    #[clap(long, short='H', default_value="127.0.0.1")]
    host: String,
    #[clap(long, short='P', default_value="9114")]
    port: u16,
    #[clap(long)]
    base_url: String,
    config: PathBuf,
}

#[derive(Clone)]
struct AppState {
    base_url: Url,
    client: reqwest::Client,
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
    async fn from_config(config: PreparedConfig, base_url: Url) -> Result<Self, AnyError> {
        let client = reqwest::Client::new();

        let mut global_labels = BTreeMap::new();
        for global_label in config.global_labels.iter() {
            let labels_url = base_url.join(&global_label.url)?;
            let labels_resp = client.get(labels_url).send().await?;
            let labels_json = serde_json::from_str(&labels_resp.text().await?)?;
            let labels_root_match = Match {
                value: &labels_json,
                path: vec!(Step::Root),
            };
            let resolved_labels = global_label.labels.resolve(&labels_root_match)?;
            global_labels.extend(resolved_labels.into_iter());
        }
        // println!("Global labels: {:?}", &global_labels);

        let root_metric = ResolvedMetric {
            metric_type: None,
            name: config.namespace.clone().unwrap_or_else(|| "".to_string()),
            labels: global_labels,
        };

        Ok(AppState {
            base_url,
            client,
            config,
            root_metric,
            buf_size: Arc::new(AsyncMutex::new(BufSize::new())),
        })
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ProcessMetricsError {
    #[error("invalid url: {0}")]
    ParseUrl(#[from] url::ParseError),
    #[error("error when sending http request: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),
}

impl ResponseError for ProcessMetricsError {
    fn error_response(&self) -> HttpResponse {
        HttpResponseBuilder::new(self.status_code())
            .body(format!("{}", self))
    }
    fn status_code(&self) -> http::StatusCode {
        http::StatusCode::INTERNAL_SERVER_ERROR
    }
}

async fn metrics(data: web::Data<AppState>) -> Result<impl Responder, ProcessMetricsError> {
    let mut request_duration = Duration::default();
    let mut json_parsing_duration = Duration::default();
    let mut processing_duration = Duration::default();

    let buf_size = {
        let buf_size = data.buf_size.lock().await;
        buf_size.buf_size()
    };

    let mut buf = Vec::with_capacity(buf_size);
    for endpoint in &data.config.endpoints {
        // TODO: make Url when preparing a config
        let endpoint_url = data.base_url.join(&endpoint.url)?;

        let start_request = Instant::now();
        let resp = data.client.get(endpoint_url).send().await?;
        let text = resp.text().await?;
        request_duration += start_request.elapsed();

        let start_parsing = Instant::now();
        let json = serde_json::from_str(&text)?;
        json_parsing_duration += start_parsing.elapsed();

        let start_processing = Instant::now();
        for (level, msg) in endpoint.metrics.process(&data.root_metric, &json, &mut buf) {
            log::log!(level, "{}", msg);
        }
        processing_duration += start_processing.elapsed();
    }

    log::info!(
        "Timings: request={}ms, parsing={}ms, processing={}ms",
        request_duration.as_millis(),
        json_parsing_duration.as_millis(),
        processing_duration.as_millis(),
    );

    let mut buf_size = data.buf_size.lock().await;
    if buf_size.seen(buf.capacity()) {
        log::info!("Set new buffer size: {}", buf_size.buf_size());
    }

    Ok(HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4")
        .body(buf))
}

#[actix_web::main]
async fn main() -> Result<(), AnyError> {
    env_logger::init();

    let opts = Opts::parse();
    let config = read_config(&opts.config)?;
    let prepared_config = PreparedConfig::create_from(&config)?;

    let base_url = Url::parse(&opts.base_url)
        .with_context(|| format!("Invalid url: {}", &opts.base_url))?;
    if base_url.query().is_some() || base_url.fragment().is_some() {
        bail!(
            "Base url must not contain query or fragment parts: {}", &base_url
        );
    }

    let app_state = loop {
        // TODO: How we can rid of those clones?
        let prepared_config = prepared_config.clone();
        let base_url = base_url.clone();
        match AppState::from_config(prepared_config, base_url).await {
            Ok(app_state) => break app_state,
            Err(e) => {
                log::error!("Error when preparing app state: {}", &e);
                delay_for(Duration::from_secs(30)).await;
                continue;
            }
        }
    };
    let app_state = Arc::new(Mutex::new(app_state));

    HttpServer::new(move || {
        // println!("Creating http application");
        let app_state = app_state.lock().expect("app state mutex lock");
        App::new()
            .wrap(middleware::Compress::default())
            .data((*app_state).clone())
            .route("/metrics", web::get().to(metrics))
    })
    .bind(format!("{}:{}", &opts.host, &opts.port))?
    .run()
    .await?;

    Ok(())
}
