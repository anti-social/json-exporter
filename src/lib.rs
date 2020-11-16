pub mod config;
pub mod convert;
mod filters;
pub mod prepare;
mod tmpl;

use anyhow::{Error as AnyError};

use fehler::throws;

use std::io::BufReader;
use std::fs::File;
use std::path::Path;

use crate::config::Config;

#[throws(AnyError)]
pub fn read_config(path: impl AsRef<Path>) -> Config {
    let config_file = BufReader::new(
        File::open(path)?
    );
    serde_yaml::from_reader(config_file)?
}