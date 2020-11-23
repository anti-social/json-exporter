use anyhow::{anyhow, bail, Error as AnyError};

use dyn_clone::DynClone;

use fehler::{throw, throws};

use serde_json::Value;

type BoxedFilter = Box<dyn Filter + Send>;

pub trait Filter: DynClone {
    fn apply(&self, value: &Value) -> Result<Value, AnyError>;
}

#[throws(AnyError)]
fn single_scalar_arg(args: &Value) -> Value {
    match args {
        Value::Number(_) | Value::String(_) | Value::Bool(_) => args.clone(),
        Value::Array(seq) => {
            match seq.as_slice() {
                [arg] => arg.clone(),
                _ => bail!("Single argument required")
            }
        }
        Value::Object(_) => bail!("Object arguments is not supported"),
        Value::Null => bail!("Single argument required"),
    }
}

#[throws(AnyError)]
fn single_arg_f64(args: &Value, map_key: Option<&str>) -> f64 {
    match args {
        Value::Number(f) => f.as_f64().unwrap(),
        Value::Array(seq) => {
            match seq.as_slice() {
                [Value::Number(ref f)] => f.as_f64().unwrap(),
                _ => bail!("Invalid argument: {:?}", args),
            }
        }
        Value::Object(map) => {
            if let Some(map_key) = map_key {
                if map.len() > 1 {
                    bail!("Too many arguments: {:?}", args);
                }
                match map.get(map_key) {
                    Some(Value::Number(f)) => f.as_f64().unwrap(),
                    _ => bail!("Invalid argument: {:?}", args),
                }
            } else {
                bail!("Keyword arguments is not supported");
            }
        }
        _ => bail!("Invalid arguments: {:?}", args),
    }
}

#[allow(dead_code)]
#[throws(AnyError)]
fn check_no_args(args: &Value) -> () {
    match args {
        Value::Array(seq) if seq.is_empty() => {}
        Value::Object(map) if map.is_empty() => {}
        Value::Null => {}
        _ => {
            bail!("Unexpected arguments");
        }
    }
}

#[derive(Clone)]
pub struct Const {
    value: Value,
}

impl Const {
    #[throws(AnyError)]
    pub fn create(args: &Value) -> BoxedFilter {
        Box::new(
            Self {
                value: single_scalar_arg(args)?,
            }
        ) as BoxedFilter
    }
}

impl Filter for Const {
    #[throws(AnyError)]
    fn apply(&self, _: &Value) -> Value {
        self.value.clone()
    }
}

#[derive(Clone)]
pub struct Multiply {
    factor: f64,
}

impl Multiply {
    #[throws(AnyError)]
    pub fn create(args: &Value) -> BoxedFilter {
        Box::new(Self {
            factor: single_arg_f64(args, Some("factor"))?
        }) as Box<dyn Filter + Send>
    }
}

impl Filter for Multiply {
    #[throws(AnyError)]
    fn apply(&self, value: &Value) -> Value {
        match value {
            Value::Number(v) => {
                Value::from(v.as_f64().unwrap() * self.factor)
            }
            _ => throw!(anyhow!("Invalid type")),
        }
    }
}

#[derive(Clone)]
pub struct Divide {
    denominator: f64,
}

impl Divide {
    #[throws(AnyError)]
    pub fn create(args: &Value) -> BoxedFilter {
        Box::new(Self {
            denominator: single_arg_f64(args, Some("divisor"))?
        }) as Box<dyn Filter + Send>
    }
}

impl Filter for Divide {
    #[throws(AnyError)]
    fn apply(&self, value: &Value) -> Value {
        match value {
            Value::Number(v) => {
                Value::from(v.as_f64().unwrap() / self.denominator)
            }
            _ => bail!("Invalid type"),
        }
    }
}

#[derive(Clone)]
pub struct Equal {
    value: Value,
}

impl Equal {
    #[throws(AnyError)]
    pub fn create(args: &Value) -> BoxedFilter {
        Box::new(Self {
            value: single_scalar_arg(args)?
        }) as Box<dyn Filter + Send>
    }
}

impl Filter for Equal {
    #[throws(AnyError)]
    fn apply(&self, value: &Value) -> Value {
        use Value::*;

        Value::from(match (&self.value, value) {
            (String(v1), String(v2)) if v1 == v2 => true,
            (Number(v1), Number(v2)) if v1 == v2 => true,
            (Bool(v1), Bool(v2)) if v1 == v2 => true,
            (Null, Null) => true,
            // TODO: Implement equality for arrays and objects
            _ => false,
        })
    }
}
