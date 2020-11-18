use anyhow::{anyhow, Error as AnyhowError};

use fehler::{throw, throws};

use jsonpath::{Selector, Match, Step};

use serde_json::Value;

use std::convert::TryFrom;

use url::Url;

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


#[derive(Clone)]
pub struct PreparedConfig {
    pub namespace: Option<String>,
    pub global_labels: Vec<PreparedGlobalLabels>,
    pub endpoints: Vec<PreparedEndpoint>,
}

impl PreparedConfig {
    #[throws(AnyhowError)]
    pub fn create_from(config: &Config, base_url: &Url) -> Self {
        let mut prepared_global_labels = vec!();
        for global_labels in &config.global_labels {
            prepared_global_labels.push(PreparedGlobalLabels::create_from(global_labels, base_url)?);
        }
        let mut prepared_endpoints = vec!();
        for endpoint in &config.endpoints {
            prepared_endpoints.push(PreparedEndpoint::create_from(endpoint, base_url)?);
        }
        Self {
            namespace: config.namespace.clone(),
            global_labels: prepared_global_labels,
            endpoints: prepared_endpoints,
        }
    }
}

#[derive(Clone)]
pub struct PreparedGlobalLabels {
    pub url: Url,
    pub labels: PreparedLabels,
}

impl PreparedGlobalLabels {
    #[throws(AnyhowError)]
    fn create_from(global_labels: &GlobalLabels, base_url: &Url) -> Self {
        Self {
            url: base_url.join(&global_labels.url)?,
            labels: PreparedLabels::try_from(&global_labels.labels)?,
        }
    }
}

#[derive(Clone)]
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
            value_processor: TemplateProcessor::create_from(&label.value)?,
        }
    }
}

#[derive(Clone)]
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


#[derive(Clone)]
pub struct PreparedEndpoint {
    pub url: Url,
    pub metrics: PreparedMetrics,
}

impl PreparedEndpoint {
    #[throws(AnyhowError)]
    fn create_from(endpoint: &Endpoint, base_url: &Url) -> Self {
        Self {
            url: base_url.join(&endpoint.url)?,
            metrics: PreparedMetrics::create_from(&endpoint.metrics, None)?
        }
    }
}

#[derive(Clone)]
pub struct PreparedMetrics(pub Vec<PreparedMetric>);

impl PreparedMetrics {
    #[throws(AnyhowError)]
    pub fn create_from(
        metrics: &[Metric],
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
    pub filters: Vec<Box<dyn PreparedFilter + Send>>,
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
        let name_processor = metric.name.as_ref().map(|n| TemplateProcessor::create_from(n))
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

impl Clone for PreparedMetric {
    fn clone(&self) -> Self {
        Self {
            selector: self.selector.clone(),
            metric_type: self.metric_type,
            name: self.name.clone(),
            name_processor: self.name_processor.clone(),
            filters: self.filters.iter()
                .map(|f| dyn_clone::clone_box(f.as_ref()))
                .collect(),
            labels: self.labels.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
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
    fn prepare(&self) -> Box<dyn PreparedFilter + Send> {
        let create_filter = match self.name.as_str() {
            "mul" | "multiply" => filters::Multiply::create,
            "div" | "divide" => filters::Divide::create,
            _ => throw!(anyhow!("Unknown filter: {}", &self.name)),
        };
        create_filter(&self.args)?
    }
}

#[derive(Clone, Default)]
pub struct TemplateProcessor {
    tmpl: Vec<PreparedPlaceholder>,
}

impl TemplateProcessor {
    #[throws(AnyhowError)]
    fn create_from(tmpl: &str) -> Self {
        if tmpl.is_empty() {
            return Default::default();
        }
        let placeholders = string_with_placeholders(tmpl).map_err(|e| {
            e.map(|e| nom::Err::Error((e.input.to_string(), e.code)))
        })?.1;
        let prepared_placeholders = placeholders.iter()
            .map(PreparedPlaceholder::create_from)
            .collect::<Result<Vec<_>, _>>()?;
        Self {
            tmpl: prepared_placeholders,
        }
    }

    #[throws(AnyhowError)]
    pub fn apply(&self, found: &Match) -> String {
        use PreparedPlaceholder::*;

        let mut text = String::new();

        // TODO: benchmark specialized versions of template processor
        for placeholder in &self.tmpl {
            match placeholder {
                Text(t) => {
                    text.push_str(t);
                }
                VarIx(path_ix) => {
                    match found.path.get(*path_ix as usize + 1) {
                        Some(Step::Key(key)) => text.push_str(key),
                        Some(Step::Index(ix)) => text.push_str(&ix.to_string()),
                        Some(Step::Root) => throw!(anyhow!("Root element is not supported")),
                        None => throw!(anyhow!("Invalid path index: {}", path_ix)),
                    }
                }
                VarIdent(selector) => {
                    // TODO: Should we return an error when there are several
                    // matching values?
                    if let Some(v) = selector.find(found.value).next() {
                        match v.value {
                            Value::String(v) => text.push_str(&v),
                            Value::Bool(v) => text.push_str(&v.to_string()),
                            Value::Number(v) => text.push_str(&v.to_string()),
                            _ => {}
                        }
                    }
                }
            }
        }
        text
    }
}

#[derive(Clone)]
enum PreparedPlaceholder {
    Text(String),
    VarIx(u32),
    VarIdent(JsonSelector),
}

impl PreparedPlaceholder {
    #[throws(AnyhowError)]
    fn create_from(placeholder: &Placeholder) -> Self {
        match placeholder {
            Placeholder::Text(text) => {
                PreparedPlaceholder::Text(text.clone())
            },
            Placeholder::Var(Var::Ix(ix)) => {
                PreparedPlaceholder::VarIx(*ix)
            },
            Placeholder::Var(Var::Ident(ident)) => {
                let selector = JsonSelector::new(ident)?;
                PreparedPlaceholder::VarIdent(selector)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    #[test]
    fn test_url() {
        let base_url = Url::parse("http://localhost/metrics/").unwrap();
        let path = "/stats?b=2";
        println!("{}", base_url.join(path).unwrap());

        // assert!(false);
    }
}