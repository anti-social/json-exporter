use actix_web::{
    web,
    App,
    HttpServer,
};

use anyhow::{bail, Context, Error as AnyError};

use clap::Clap;

use json_exporter::read_config;
use json_exporter::prepare::PreparedConfig;
use json_exporter::service::{
    AppState,
    info,
    metrics,
    resolve_global_labels,
};

use mimalloc::MiMalloc;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::delay_for;

use url::Url;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

const GLOBAL_LABELS_RETRY_INTERVAL_SECS: u64 = 30;

#[derive(Clap, Debug)]
struct Opts {
    #[clap(long, short='H', default_value="127.0.0.1")]
    host: String,
    #[clap(long, short='P', default_value="9114")]
    port: u16,
    #[clap(long)]
    base_url: String,
    #[clap(long)]
    endpoint_url: Vec<String>,
    #[clap(long, default_value="10000")]
    timeout_ms: u32,
    #[clap(long)]
    namespace: Option<String>,
    config: PathBuf,
}

fn parse_endpoint_url(url_dsl: &str) -> Result<(String, String), AnyError> {
    Ok(match &url_dsl.splitn(2, ':').collect::<Vec<_>>()[..] {
        [""] => bail!("Missing endpoint id"),
        [_] => bail!("Missing endpoint url"),
        [endpoint_id, endpoint_url_dsl] => {
            (endpoint_id.to_string(), endpoint_url_dsl.to_string())
        },
        _ => unreachable!(),
    })
}

#[actix_web::main]
async fn main() -> Result<(), AnyError> {
    env_logger::init();

    let opts = Opts::parse();

    let mut base_url = Url::parse(&opts.base_url)
        .with_context(|| format!("Invalid url: {}", &opts.base_url))?;
    if base_url.query().is_some() || base_url.fragment().is_some() {
        bail!(
            "Base url must not contain query or fragment parts: {}", &base_url
        );
    }
    if !base_url.path().ends_with('/') {
        let mut base_url_path_segments = match base_url.path_segments_mut() {
            Ok(segments) => segments,
            Err(()) => bail!("Not a base url"),
        };
        base_url_path_segments.push("");
    }

    let endpoint_urls = opts.endpoint_url.iter()
        .map(String::as_str)
        .map(parse_endpoint_url)
        .collect::<Result<HashMap<_, _>, _>>()?;
    let timeout = Duration::from_millis(opts.timeout_ms as u64);
    let config = read_config(&opts.config)?;
    let prepared_config = PreparedConfig::create_from(
        &config, &base_url, &endpoint_urls
    )?;


    for global_label in &prepared_config.global_labels {
        log::info!("Global labels url: {}", &global_label.url);
    }
    for endpoint in &prepared_config.endpoints {
        if let Some(endpoint_id) = &endpoint.id {
            log::info!("Endpoint url [{}]: {}", endpoint_id, &endpoint.url);
        } else {
            log::info!("Endpoint url: {}", &endpoint.url);
        }
    }

    let client = reqwest::Client::new();
    let app_state = loop {
        // TODO: How we can rid of those clones?
        let prepared_config = prepared_config.clone();
        let base_url = base_url.clone();
        match resolve_global_labels(&prepared_config, &client, timeout).await {
            Ok(labels) => {
                break AppState::new(
                    prepared_config, opts.namespace, labels, client, base_url, timeout
                );
            },
            Err(e) => {
                log::error!("Error when resolving global labels: {}", e);
                log::warn!(
                    "Waiting {} seconds before retry",
                    GLOBAL_LABELS_RETRY_INTERVAL_SECS
                );
                delay_for(
                    Duration::from_secs(GLOBAL_LABELS_RETRY_INTERVAL_SECS)
                ).await;
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
            .route("/", web::get().to(info))
            .route("/metrics", web::get().to(metrics))
    })
    .bind(format!("{}:{}", &opts.host, &opts.port))?
    .run()
    .await?;

    Ok(())
}
