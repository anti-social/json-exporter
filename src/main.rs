use actix_web::{
    http,
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

use jsonpath::{Match, Step};

use tokio::time::delay_for;

use url::Url;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use json_exporter::read_config;
use json_exporter::prepare::PreparedConfig;
use json_exporter::convert::ResolvedMetric;

#[derive(Clap, Debug)]
struct Opts {
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
            name: config.namespace.clone().unwrap_or("".to_string()),
            labels: global_labels,
        };

        Ok(AppState {
            base_url,
            client,
            config,
            root_metric,
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
    let mut buf = vec!();
    for endpoint in &data.config.endpoints {
        // TODO: make Url when preparing a config
        let endpoint_url = data.base_url.join(&endpoint.url)?;
        let resp = data.client.get(endpoint_url).send().await?;
        let json = serde_json::from_str(&resp.text().await?)?;

        endpoint.metrics.process(&data.root_metric, &json, &mut buf);
    }
    Ok(HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4")
        .body(buf))
}

#[actix_web::main]
async fn main() -> Result<(), AnyError> {
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
                // TODO: log.error
                println!("Error when preparing app state: {}", &e);
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
            .data((*app_state).clone())
            .route("/metrics", web::get().to(metrics))
    })
    .bind("127.0.0.1:9114")?
    .run()
    .await?;

    Ok(())
}
