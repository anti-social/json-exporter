use anyhow::Error as AnyhowError;

use fehler::throws;

use serde::{Deserialize, Deserializer};
use serde::de::{Visitor, SeqAccess};

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use void::Void;

use crate::prepare::PreparedConfig;


#[derive(Deserialize)]
pub struct Config {
    pub namespace: Option<String>,
    pub global_labels: Vec<GlobalLabels>,
    pub endpoints: Vec<Endpoint>,
}

impl Config {
    #[throws(AnyhowError)]
    pub fn prepare(&self) -> PreparedConfig {
        PreparedConfig::create_from(self)?
    }
}

#[derive(Deserialize)]
pub struct GlobalLabels {
    pub url: String,
    pub labels: Vec<Label>,
}

#[derive(Deserialize)]
pub struct Label {
    pub name: String,
    pub value: String,
}

#[derive(Deserialize)]
pub struct Endpoint {
    pub url: String,
    #[serde(deserialize_with = "deserialize_metrics")]
    pub metrics: Vec<Metric>,
}

#[derive(Deserialize)]
pub struct Metrics {
    #[serde(deserialize_with = "deserialize_metrics")]
    pub metrics: Vec<Metric>,
}

#[derive(Deserialize, Default)]
pub struct Metric {
    pub path: String,
    pub name: Option<String>,
    #[serde(rename = "type", default)]
    pub metric_type: Option<MetricType>,
    #[serde(default)]
    pub modifiers: Vec<Filter>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_metrics")]
    pub metrics: Vec<Metric>,
}

impl FromStr for Metric {
    type Err = Void;

    #[throws(Self::Err)]
    fn from_str(s: &str) -> Self {
        Metric {
            path: s.to_string(),
            ..Default::default()
        }
    }
}

fn deserialize_metrics<'de, D>(deserializer: D) -> Result<Vec<Metric>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MetricOrPath {
        Metric(Metric),
        Path(String),
    }

    struct MetricsVisitor(PhantomData<fn() -> Metric>);

    impl<'de> Visitor<'de> for MetricsVisitor {
        type Value = Vec<Metric>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("sequence")
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: SeqAccess<'de>,
        {
            let mut metrics = vec!();
            while let Some(v) = seq.next_element::<MetricOrPath>()? {
                let metric = match v {
                    MetricOrPath::Metric(m) => m,
                    MetricOrPath::Path(p) => Metric::from_str(&p).unwrap(),
                };
                metrics.push(metric);
            }
            Ok(metrics)
        }
    }

    deserializer.deserialize_any(MetricsVisitor(PhantomData))
}

#[derive(Deserialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum MetricType {
    Gauge,
    Counter,
    Untyped,
}

#[derive(Deserialize)]
pub struct Filter {
    pub name: String,
    pub args: serde_yaml::Value,
}


#[cfg(test)]
mod tests {
    use serde_yaml;
    use std::fs::File;
    use std::io::BufReader;
    use crate::config::Config;

    #[test]
    fn test_elasticsearch_exporter_config() {
        let filename = "elasticsearch_exporter.yaml";
        let file = BufReader::new(
            File::open(filename).expect(filename)
        );
        let _config: Config = serde_yaml::from_reader(file).unwrap();
    }
}
