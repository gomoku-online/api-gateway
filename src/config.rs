use anyhow::Result;
use config::{Config, File, FileFormat};
use serde::Deserialize;
use std::{env, fs};
use regex::Regex;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub logging: LoggingConfig,
    pub gateway: GatewayConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub host: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GatewayConfig {
    pub routes: Vec<Route>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Route {
    pub id: String,
    pub uri: String,
    pub predicates: Vec<Predicate>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub enum Predicate {
    Path(String),
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let run_mode = env::var("RUN_MODE").unwrap_or_else(|_| "local".into());

        let base_yaml = fs::read_to_string("config/application.yml")?;
        let base_yaml = Self::substitute_env_vars(&base_yaml);

        let mode_path = format!("config/application-{}.yml", run_mode);
        let mode_yaml = fs::read_to_string(&mode_path).ok();
        let mode_yaml = mode_yaml.map(|yml| Self::substitute_env_vars(&yml));

        let mut builder =
            Config::builder().add_source(File::from_str(&base_yaml, FileFormat::Yaml));

        if let Some(mode_yaml) = mode_yaml {
            builder = builder.add_source(File::from_str(&mode_yaml, FileFormat::Yaml));
        }

        let config = builder.build()?.try_deserialize()?;
        Ok(config)
    }

    fn substitute_env_vars(content: &str) -> String {
        let re = Regex::new(r"\$\{(\w+)(:([^}]*))?\}").unwrap();
        re.replace_all(content, |caps: &regex::Captures| {
            let var_name = &caps[1];
            let default = caps.get(3).map_or("", |m| m.as_str());

            std::env::var(var_name).unwrap_or_else(|_| default.to_string())
        })
            .to_string()
    }
}