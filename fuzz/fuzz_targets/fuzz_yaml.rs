use honggfuzz::fuzz;

use std::fs;
use std::path::PathBuf;

use ffs::{
    config::{Config, Munge, Output, Input},
    format::{Format, yaml::Value},
    fs::FS
};

fn main() {
    loop {
        fuzz!(|data: &[u8]| {
            if let Ok(src) = std::str::from_utf8(data) {
                // Write data to a file
                fs::write("temp.toml", src).unwrap();
    
                // Create a config
                let mut config = Config::default();
                config.input = Input::File(PathBuf::from("temp.toml"));
                config.munge = Munge::Filter;
                config.output = Output::Quiet;
                config.input_format = Format::Yaml;
                config.output_format = Format::Yaml;
    
                // Create a FS
                let _: FS<Value> = FS::new(config);
            }
        });
    }
}