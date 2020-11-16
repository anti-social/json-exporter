use anyhow::{Error as AnyError};

use clap::Clap;

use jsonpath::{Match, Step};

use reqwest::Url;

use std::collections::BTreeMap;
use std::path::PathBuf;

use json_exporter::read_config;
use json_exporter::prepare::PreparedConfig;
use json_exporter::convert::ResolvedMetric;

#[derive(Clap, Debug)]
struct Opts {
    #[clap(long)]
    es_url: String,
    config: PathBuf,
}


#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let opts = Opts::parse();
    let config = read_config(&opts.config)?;
    let prepared_config = PreparedConfig::create_from(&config)?;

    let es_url = Url::parse(&opts.es_url)?;
    let client = reqwest::Client::new();

    let mut global_labels = BTreeMap::new();
    for global_label in  prepared_config.global_labels.iter() {
        let labels_url = es_url.join(&global_label.url)?;
        let labels_resp = client.get(labels_url).send().await?;
        let labels_json = serde_json::from_str(&labels_resp.text().await?)?;
        let labels_root_match = Match {
            value: &labels_json,
            path: vec!(Step::Root),
        };
        let resolved_labels = global_label.labels.resolve(&labels_root_match);
        global_labels.extend(resolved_labels.into_iter());
    }
    // println!("Global labels: {:?}", &global_labels);

    let root_metric = ResolvedMetric {
        metric_type: None,
        name: prepared_config.namespace.clone().unwrap_or("".to_string()),
        labels: global_labels,
    };

    let mut buf = vec!();
    for endpoint in &prepared_config.endpoints {
        let endpoint_url = es_url.join(&endpoint.url)?;
        let resp = client.get(endpoint_url).send().await?;
        let json = serde_json::from_str(&resp.text().await?)?;

        endpoint.metrics.process(&root_metric, &json, &mut buf);
    }

    println!("{}", String::from_utf8(buf)?);

    Ok(())
}
