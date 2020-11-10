use anyhow::{anyhow, Error as AnyhowError};

use fehler::throws;

use jsonpath::{Selector, Found, Step};

use serde::Deserialize;
use serde_json::Value;

use crate::tmpl::{
    string_with_placeholders,
    Placeholder,
    Var,
};


type TemplateProcessor = Box<dyn Fn(&Found) -> Option<String>>;

#[derive(Deserialize)]
pub struct Metric {
    name: String,
    #[serde(rename = "type", default)]
    metric_type: Option<MetricType>,
    selector: String,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    metrics: Vec<Metric>,
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

#[derive(Deserialize)]
pub struct Metrics {
    metrics: Vec<Metric>,
}

impl Metrics {
    #[throws(AnyhowError)]
    pub fn prepare(&self) -> PreparedMetrics {
        PreparedMetrics(PreparedMetrics::from_metrics(&self.metrics, None)?)
    }
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
    pub metric_type: Option<MetricType>,
    pub name: String,
    pub name_processor: TemplateProcessor,
    pub selector: JsonSelector,
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
        let name_processor = make_value_processor(&metric.name)?;
        let selector = JsonSelector::new(&metric.selector)?;
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
    use crate::config::Metrics;

    #[test]
    fn test_parsing_config() {
        let _metrics: Metrics = serde_yaml::from_str(indoc! {"
          metrics:
          - name: test
            selector: asdf
            metrics:
            - name: 1234
              selector: fdsa.*
              labels:
              - name: test
                value: $0

        "})
            .expect("valid yaml");
    }
}
