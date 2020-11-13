use anyhow::{anyhow, Error as AnyhowError};

use fehler::throws;

use jsonpath::{Selector, Found, Step};

use serde::{Deserialize, Deserializer};
use serde::de::{Visitor, SeqAccess};
use serde_json::Value;

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use void::Void;

use crate::tmpl::{
    string_with_placeholders,
    Placeholder,
    Var,
};


type TemplateProcessor = Box<dyn Fn(&Found) -> Option<String>>;

#[derive(Deserialize)]
pub struct Config {
    namespace: Option<String>,
    global_labels: Vec<GlobalLabel>,
    endpoints: Vec<Endpoint>,
}

#[derive(Deserialize)]
pub struct GlobalLabel {
    url: String,
    labels: Vec<Label>,
}

#[derive(Deserialize)]
pub struct Endpoint {
    url: String,
    #[serde(flatten)]
    metrics: Metrics,
}

#[derive(Deserialize)]
pub struct Metrics {
    name: Option<String>,
    #[serde(deserialize_with = "deserialize_metrics")]
    metrics: Vec<Metric>,
}

impl Metrics {
    #[throws(AnyhowError)]
    pub fn prepare(&self) -> PreparedMetrics {
        PreparedMetrics(PreparedMetrics::from_metrics(&self.metrics, None)?)
    }
}

#[derive(Deserialize, Default)]
pub struct Metric {
    path: String,
    name: Option<String>,
    #[serde(rename = "type", default)]
    metric_type: Option<MetricType>,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_metrics")]
    metrics: Vec<Metric>,
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
pub struct Label {
    name: String,
    value: String,
}

pub struct PreparedMetrics(pub(crate) Vec<PreparedMetric>);

impl PreparedMetrics {
    #[throws(AnyhowError)]
    fn from_metrics(
        metrics: &Vec<Metric>,
        metric_type: Option<MetricType>
    ) -> Vec<PreparedMetric> {
        let mut prepared_metrics = vec!();
        for metric in metrics {
            prepared_metrics.push(PreparedMetric::from_metric(metric, metric_type)?);
        }
        prepared_metrics
    }

    pub fn iter(&self) -> std::slice::Iter<PreparedMetric> {
        self.0.iter()
    }
}

pub struct PreparedMetric {
    pub selector: JsonSelector,
    pub metric_type: Option<MetricType>,
    pub name: Option<String>,
    pub name_processor: Option<TemplateProcessor>,
    pub labels: Vec<PreparedLabel>,
    pub metrics: Vec<PreparedMetric>
}

impl PreparedMetric {
    #[throws(AnyhowError)]
    fn from_metric(
        metric: &Metric,
        parent_metric_type: Option<MetricType>,
    ) -> Self {
        let metric_type = metric.metric_type.or(parent_metric_type);
        // TODO: validate matric and label names
        let name = metric.name.clone();
        let name_processor = metric.name.as_ref().map(|n| make_value_processor(n))
            .transpose()?;
        let selector = JsonSelector::new(&metric.path)?;
        let mut prepared_labels = vec!();
        for label in &metric.labels {
            prepared_labels.push(PreparedLabel::from_label(label)?);
        }
        Self {
            metric_type,
            name,
            name_processor,
            selector,
            labels: prepared_labels,
            metrics: PreparedMetrics::from_metrics(&metric.metrics, metric_type)?,
        }
    }
}

pub struct JsonSelector {
    pub expression: String,
    selector: Selector,
}

trait Captures<'a> { }
impl<'a, T: ?Sized> Captures<'a> for T { }

impl JsonSelector {
    #[throws(AnyhowError)]
    fn new(expression: &str) -> Self {
        let expression = if expression.is_empty() {
            "$".to_string()
        } else {
            format!("$.{}", expression)
        };
        let selector = Selector::new(&expression)
            .map_err(|e| anyhow!(
            "Error when creating json selector [{}]: {}", expression, e
        ))?;

        Self {
            expression,
            selector,
        }
    }

    pub fn find<'a>(&'a self, root: &'a Value) -> impl Iterator<Item=Found<'_>> {
        self.selector.find(root)
    }
}

pub struct PreparedLabel {
    pub name: String,
    pub value_processor: TemplateProcessor,
}

impl PreparedLabel {
    #[throws(AnyhowError)]
    fn from_label(label: &Label) -> Self {
        PreparedLabel {
            name: label.name.clone(),
            value_processor: make_value_processor(&label.value)?,
        }
    }
}

fn make_value_processor(tmpl: &str) -> Result<TemplateProcessor, AnyhowError> {
    if tmpl.is_empty() {
        let value = tmpl.to_string();
        return Ok(Box::new(move |_| Some(value.clone())));
    }

    let placeholders = string_with_placeholders(tmpl).map_err(|e| {
        e.map(|e| nom::Err::Error((e.input.to_string(), e.code)))
    })?.1;
    let processor: TemplateProcessor = match &placeholders[..] {
        [] => {
            let value = tmpl.to_string();
            Box::new(move |_| Some(value.clone()))
        }
        [Placeholder::Text(text)] => {
            let text = text.clone();
            Box::new(move |_| Some(text.clone()))
        }
        [Placeholder::Var(Var::Ix(path_ix))] => {
            let path_ix = *path_ix;
            Box::new(move |found| {
                match found.path[path_ix as usize + 1] {
                    Step::Key(key) => Some(key.to_string()),
                    Step::Index(ix) => Some(ix.to_string()),
                    Step::Root => panic!(),
                }
            })
        }
        [Placeholder::Var(Var::Ident(ident))] => {
            let selector = JsonSelector::new(ident)?;
            Box::new(move |found| {
                selector.find(found.value)
                    .next()
                    .map(|found_value| found_value.value)
                    .and_then(|v| match v {
                        Value::String(v) => Some(v.clone()),
                        Value::Bool(v) => Some(v.to_string()),
                        Value::Number(v) => Some(v.to_string()),
                        _ => None,
                    })
            })
        }
        placeholders => {
            let placeholders = placeholders.to_vec();
            Box::new(move |found| {
                let mut text = String::new();
                for placeholder in &placeholders {
                    match placeholder {
                        Placeholder::Text(t) => {
                            text.push_str(t);
                        }
                        Placeholder::Var(Var::Ix(path_ix)) => {
                            match found.path[*path_ix as usize + 1] {
                                Step::Key(key) => text.push_str(key),
                                Step::Index(ix) => text.push_str(&ix.to_string()),
                                Step::Root => panic!(),
                            }
                        }
                        Placeholder::Var(Var::Ident(ident)) => {
                            // FIXME: rid of unwrap
                            let selector = JsonSelector::new(ident).unwrap();
                            selector.find(found.value)
                                .next()
                                .map(|found_value| found_value.value)
                                .map(|v| match v {
                                    Value::String(v) => text.push_str(&v),
                                    Value::Bool(v) => text.push_str(&v.to_string()),
                                    Value::Number(v) => text.push_str(&v.to_string()),
                                    _ => {},
                                });
                        }
                    }
                }
                Some(text)
            })
        }
    };
    Ok(processor)
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use serde_yaml;
    use std::fs::File;
    use crate::config::{Config, Metrics};

    #[test]
    fn test_parsing_config() {
        let _metrics: Metrics = serde_yaml::from_str(indoc! {"
          metrics:
          - path: asdf
            name: test
            metrics:
            - path: fdsa
              name: '1234'
        "})
            .expect("valid yaml");
    }

    #[test]
    fn test_elasticsearch_exporter_config() {
        let filename = "elasticsearch_exporter.yaml";
        let file = File::open(filename).expect(filename);
        let _config: Config = serde_yaml::from_reader(file).unwrap();
    }
}
