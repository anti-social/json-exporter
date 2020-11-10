use anyhow::Error;

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
    selector: String,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    metrics: Vec<Metric>,
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
    #[throws(Error)]
    pub fn prepare(&self) -> PreparedMetrics {
        PreparedMetrics(PreparedMetrics::from_metrics(&self.metrics)?)
    }
}

pub struct PreparedMetrics(pub(crate) Vec<PreparedMetric>);

impl PreparedMetrics {
    #[throws(Error)]
    fn from_metrics(metrics: &Vec<Metric>) -> Vec<PreparedMetric> {
        let mut prepared_metrics = vec!();
        for metric in metrics {
            prepared_metrics.push(PreparedMetric::from_metric(metric)?);
        }
        prepared_metrics
    }

    pub fn iter(&self) -> std::slice::Iter<PreparedMetric> {
        self.0.iter()
    }
}

pub struct PreparedMetric {
    pub name: String,
    pub name_processor: TemplateProcessor,
    pub selector_expression: String,
    pub selector: Selector,
    pub labels: Vec<PreparedLabel>,
    pub metrics: Vec<PreparedMetric>
}

impl PreparedMetric {
    #[throws(Error)]
    fn from_metric(metric: &Metric) -> Self {
        // TODO: validate matric and label names
        let name = metric.name.clone();
        let name_processor = make_value_processor(&metric.name)?;
        let selector_expression = format!("$.{}", metric.selector);
        let selector = Selector::new(&selector_expression).unwrap();
        let mut prepared_labels = vec!();
        for label in &metric.labels {
            prepared_labels.push(PreparedLabel::from_label(label)?);
        }
        Self {
            name,
            name_processor,
            selector_expression,
            selector,
            labels: prepared_labels,
            metrics: PreparedMetrics::from_metrics(&metric.metrics)?,
        }
    }
}

pub struct PreparedLabel {
    pub name: String,
    pub value_processor: TemplateProcessor,
}

impl PreparedLabel {
    #[throws(Error)]
    fn from_label(label: &Label) -> Self {
        PreparedLabel {
            name: label.name.clone(),
            value_processor: make_value_processor(&label.value)?,
        }
    }
}

fn make_value_processor(tmpl: &str) -> Result<TemplateProcessor, Error> {
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
            let selector = Selector::new(&format!("$.{}", ident)).unwrap();
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
                            let selector = Selector::new(&format!("$.{}", ident)).unwrap();
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
