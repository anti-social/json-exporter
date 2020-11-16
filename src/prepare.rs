use anyhow::{anyhow, Error as AnyhowError};

use fehler::{throw, throws};

use jsonpath::{Selector, Match, Step};

use serde_json::Value;

use std::convert::TryFrom;

use crate::config::{
    Config,
    Endpoint,
    Filter,
    GlobalLabels,
    Label,
    Metric,
    MetricType,
};
use crate::filters::{
    self,
    Filter as PreparedFilter,
};
use crate::tmpl::{
    string_with_placeholders,
    Placeholder,
    Var,
};


type TemplateProcessor = Box<dyn Fn(&Match) -> Option<String>>;

pub struct PreparedConfig {
    pub namespace: Option<String>,
    pub global_labels: Vec<PreparedGlobalLabels>,
    pub endpoints: Vec<PreparedEndpoint>,
}

impl PreparedConfig {
    #[throws(AnyhowError)]
    pub fn create_from(config: &Config) -> Self {
        let mut prepared_global_labels = vec!();
        for global_labels in &config.global_labels {
            prepared_global_labels.push(PreparedGlobalLabels::try_from(global_labels)?);
        }
        let mut prepared_endpoints = vec!();
        for endpoint in &config.endpoints {
            prepared_endpoints.push(PreparedEndpoint::create_from(endpoint)?);
        }
        Self {
            namespace: config.namespace.clone(),
            global_labels: prepared_global_labels,
            endpoints: prepared_endpoints,
        }
    }
}

pub struct PreparedGlobalLabels {
    pub url: String,
    pub labels: PreparedLabels,
}

impl<'a> TryFrom<&'a GlobalLabels> for PreparedGlobalLabels {
    type Error = AnyhowError;

    #[throws(Self::Error)]
    fn try_from(global_labels: &GlobalLabels) -> Self {
        Self {
            url: global_labels.url.clone(),
            labels: PreparedLabels::try_from(&global_labels.labels)?,
        }
    }
}

pub struct PreparedLabel {
    pub name: String,
    pub value_processor: TemplateProcessor,
}

impl<'a> TryFrom<&'a Label> for PreparedLabel {
    type Error = AnyhowError;

    #[throws(AnyhowError)]
    fn try_from(label: &Label) -> Self {
        Self {
            name: label.name.clone(),
            value_processor: make_value_processor(&label.value)?,
        }
    }
}

pub struct PreparedLabels {
    pub(crate) labels: Vec<PreparedLabel>,
}

impl<'a> TryFrom<&'a Vec<Label>> for PreparedLabels {
    type Error = AnyhowError;

    #[throws(AnyhowError)]
    fn try_from(labels: &'a Vec<Label>) -> Self {
        let mut prepared_labels = vec!();
        for label in labels {
            prepared_labels.push(PreparedLabel::try_from(label)?);
        }
        Self { labels: prepared_labels }
    }
}

pub struct PreparedEndpoint {
    pub url: String,
    pub metrics: PreparedMetrics,
}

impl PreparedEndpoint {
    #[throws(AnyhowError)]
    fn create_from(endpoint: &Endpoint) -> Self {
        Self {
            url: endpoint.url.clone(),
            metrics: PreparedMetrics::create_from(&endpoint.metrics, None)?
        }
    }
}

pub struct PreparedMetrics(pub Vec<PreparedMetric>);

impl PreparedMetrics {
    #[throws(AnyhowError)]
    pub fn create_from(
        metrics: &Vec<Metric>,
        metric_type: Option<MetricType>
    ) -> Self {
        let mut prepared_metrics = vec!();
        for metric in metrics.iter() {
            prepared_metrics.push(PreparedMetric::create_from(metric, metric_type)?);
        }
        Self(prepared_metrics)
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
    pub filters: Vec<Box<dyn PreparedFilter>>,
    pub labels: PreparedLabels,
    pub metrics: PreparedMetrics,
}

impl PreparedMetric {
    #[throws(AnyhowError)]
    fn create_from(
        metric: &Metric,
        parent_metric_type: Option<MetricType>,
    ) -> Self {
        let metric_type = metric.metric_type.or(parent_metric_type);
        // TODO: validate metric and label names
        let name = metric.name.clone();
        let name_processor = metric.name.as_ref().map(|n| make_value_processor(n))
            .transpose()?;
        let selector = JsonSelector::new(&metric.path)?;

        let mut prepared_filters = vec!();
        for filter in &metric.modifiers {
            prepared_filters.push(filter.prepare()?);
        }

        Self {
            metric_type,
            name,
            name_processor,
            selector,
            filters: prepared_filters,
            labels: PreparedLabels::try_from(&metric.labels)?,
            metrics: PreparedMetrics::create_from(&metric.metrics, metric_type)?,
        }
    }
}

pub struct JsonSelector {
    pub expression: String,
    selector: Selector,
}

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

    pub fn find<'a>(&'a self, root: &'a Value) -> impl Iterator<Item=Match<'_>> {
        self.selector.find(root)
    }
}

impl Filter {
    #[throws(AnyhowError)]
    fn prepare(&self) -> Box<dyn PreparedFilter> {
        let create_filter = match self.name.as_str() {
            "mul" | "multiply" => filters::Multiply::create,
            "div" | "divide" => filters::Divide::create,
            _ => throw!(anyhow!("Unknown filter: {}", &self.name)),
        };
        create_filter(&self.args)?
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
