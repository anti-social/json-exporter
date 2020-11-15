use anyhow::{anyhow, Error as AnyhowError};

use fehler::{throw, throws};

use serde_json::{Value as JsonValue};
use serde_yaml::{Value as YamlValue};

pub trait Filter {
    fn apply(&self, value: &JsonValue) -> Result<JsonValue, AnyhowError>;
}

#[throws(AnyhowError)]
fn single_arg_f64(args: &YamlValue, map_key: &str) -> f64 {
    match args {
        YamlValue::Number(f) => f.as_f64().unwrap(),
        YamlValue::Sequence(seq) => {
            match seq.as_slice() {
                &[YamlValue::Number(ref f)] => f.as_f64().unwrap(),
                _ => throw!(anyhow!("Invalid argument: {:?}", args)),
            }
        }
        YamlValue::Mapping(map) => {
            if map.len() > 1 {
                throw!(anyhow!("Too many arguments: {:?}", args));
            }
            match map.get(&YamlValue::from(map_key)) {
                Some(YamlValue::Number(f)) => f.as_f64().unwrap(),
                _ => throw!(anyhow!("Invalid argument: {:?}", args)),
            }
        }
        _ => throw!(anyhow!("Invalid arguments: {:?}", args)),
    }
}

pub struct Multiply {
    factor: f64,
}

impl Multiply {
    #[throws(AnyhowError)]
    pub fn create(args: &YamlValue) -> Box<dyn Filter> {
        Box::new(Self {
            factor: single_arg_f64(args, "factor")?
        }) as Box<dyn Filter>
    }
}

impl Filter for Multiply {
    #[throws(AnyhowError)]
    fn apply(&self, value: &JsonValue) -> JsonValue {
        match value {
            JsonValue::Number(v) => {
                JsonValue::from(v.as_f64().unwrap() * self.factor)
            }
            _ => throw!(anyhow!("Invalid type")),
        }
    }
}

pub struct Divide {
    denominator: f64,
}

impl Divide {
    #[throws(AnyhowError)]
    pub fn create(args: &YamlValue) -> Box<dyn Filter> {
        Box::new(Self {
            denominator: single_arg_f64(args, "divisor")?
        }) as Box<dyn Filter>
    }
}

impl Filter for Divide {
    #[throws(AnyhowError)]
    fn apply(&self, value: &JsonValue) -> JsonValue {
        match value {
            JsonValue::Number(v) => {
                JsonValue::from(v.as_f64().unwrap() / self.denominator)
            }
            _ => throw!(anyhow!("Invalid type")),
        }
    }
}
