use anyhow::{anyhow, Error};
use cln_plugin::{options, ConfiguredPlugin};
use log::warn;
use std::path::Path;

use tokio::fs;

use crate::model::PluginState;

// pub fn validateargs(args: serde_json::Value, mut config: Config) -> Result<Config, Error> {
//     match args {
//         serde_json::Value::Object(i) => {
//             for arg in i.iter() {
//                 match arg.0 {
//                     name if name.eq(&config.utf8.0) => match arg.1 {
//                         serde_json::Value::Bool(b) => config.utf8.1 = *b,
//                         _ => {
//                             return Err(anyhow!(
//                                 "Error: {} needs to be bool (true or false).",
//                                 config.utf8.0
//                             ))
//                         }
//                     },
//                     other => return Err(anyhow!("option not found:{:?}", other)),
//                 };
//             }
//         }
//         _ => (),
//     };

//     Ok(config)
// }

pub async fn read_config(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut configfile = String::new();
    let dir = plugin.clone().configuration().lightning_dir;
    match fs::read_to_string(Path::new(&dir).join("config")).await {
        Ok(file) => configfile = file,
        Err(_) => {
            match fs::read_to_string(Path::new(&dir).parent().unwrap().join("config")).await {
                Ok(file2) => configfile = file2,
                Err(_) => warn!("No config file found!"),
            }
        }
    }
    let mut config = state.config.lock();
    for line in configfile.lines() {
        if line.contains('=') {
            let splitline = line.split('=').collect::<Vec<&str>>();
            if splitline.len() == 2 {
                let name = splitline.clone().into_iter().nth(0).unwrap();
                let value = splitline.into_iter().nth(1).unwrap();

                match name {
                    opt if opt.eq(&config.utf8.0) => match value.parse::<bool>() {
                        Ok(b) => config.utf8.1 = b,
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse bool from `{}` for {}: {}",
                                value,
                                config.utf8.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.refresh_peers_interval.0) => match value.parse::<u64>() {
                        Ok(n) => {
                            if n > 0 {
                                config.refresh_peers_interval.1 = n
                            } else {
                                return Err(anyhow!(
                                    "Error: Number needs to be greater than 0 for {}.",
                                    config.refresh_peers_interval.0
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.refresh_peers_interval.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.refresh_aliasmap_interval.0) => {
                        match value.parse::<u64>() {
                            Ok(n) => {
                                if n > 0 {
                                    config.refresh_aliasmap_interval.1 = n
                                } else {
                                    return Err(anyhow!(
                                        "Error: Number needs to be greater than 0 for {}.",
                                        config.refresh_aliasmap_interval.0
                                    ));
                                }
                            }
                            Err(e) => {
                                return Err(anyhow!(
                                    "Error: Could not parse a positive number from `{}` for {}: {}",
                                    value,
                                    config.refresh_aliasmap_interval.0,
                                    e
                                ))
                            }
                        }
                    }
                    opt if opt.eq(&config.refresh_graph_interval.0) => match value.parse::<u64>() {
                        Ok(n) => {
                            if n > 0 {
                                config.refresh_graph_interval.1 = n
                            } else {
                                return Err(anyhow!(
                                    "Error: Number needs to be greater than 0 for {}.",
                                    config.refresh_graph_interval.0
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.refresh_graph_interval.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.refresh_liquidity_interval.0) => {
                        match value.parse::<u64>() {
                            Ok(n) => {
                                if n > 0 {
                                    config.refresh_liquidity_interval.1 = n
                                } else {
                                    return Err(anyhow!(
                                        "Error: Number needs to be greater than 0 for {}.",
                                        config.refresh_liquidity_interval.0
                                    ));
                                }
                            }
                            Err(e) => {
                                return Err(anyhow!(
                                    "Error: Could not parse a positive number from `{}` for {}: {}",
                                    value,
                                    config.refresh_liquidity_interval.0,
                                    e
                                ))
                            }
                        }
                    }
                    _ => (),
                }
            }
        }
    }

    Ok(())
}

pub fn get_startup_options(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut config = state.config.lock();
    config.utf8.1 = match plugin.option(&config.utf8.0) {
        Some(options::Value::Boolean(b)) => b,
        Some(_) => config.utf8.1,
        None => config.utf8.1,
    };
    config.refresh_peers_interval.1 = match plugin.option(&config.refresh_peers_interval.0) {
        Some(options::Value::Integer(i)) => {
            if i > 0 {
                i as u64
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than 0 and not `{}`.",
                    config.refresh_peers_interval.0,
                    i
                ));
            }
        }
        Some(_) => config.refresh_peers_interval.1,
        None => config.refresh_peers_interval.1,
    };
    config.refresh_aliasmap_interval.1 = match plugin.option(&config.refresh_aliasmap_interval.0) {
        Some(options::Value::Integer(i)) => {
            if i > 0 {
                i as u64
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than 0 and not `{}`.",
                    config.refresh_aliasmap_interval.0,
                    i
                ));
            }
        }
        Some(_) => config.refresh_aliasmap_interval.1,
        None => config.refresh_aliasmap_interval.1,
    };
    config.refresh_graph_interval.1 = match plugin.option(&config.refresh_graph_interval.0) {
        Some(options::Value::Integer(i)) => {
            if i > 0 {
                i as u64
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than 0 and not `{}`.",
                    config.refresh_graph_interval.0,
                    i
                ));
            }
        }
        Some(_) => config.refresh_graph_interval.1,
        None => config.refresh_graph_interval.1,
    };
    config.refresh_liquidity_interval.1 = match plugin.option(&config.refresh_liquidity_interval.0)
    {
        Some(options::Value::Integer(i)) => {
            if i > 0 {
                i as u64
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than 0 and not `{}`.",
                    config.refresh_liquidity_interval.0,
                    i
                ));
            }
        }
        Some(_) => config.refresh_liquidity_interval.1,
        None => config.refresh_liquidity_interval.1,
    };

    Ok(())
}
