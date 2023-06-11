use anyhow::{anyhow, Error};
use cln_plugin::{options, ConfiguredPlugin};
use log::warn;
use std::path::Path;

use tokio::fs;

use crate::model::PluginState;

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
                    opt if opt.eq(&config.cltv_delta.0) => match value.parse::<u16>() {
                        Ok(n) => config.cltv_delta.1 = Some(n),
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a number from `{}` for {}: {}",
                                value,
                                config.cltv_delta.0,
                                e
                            ))
                        }
                    },
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
                    opt if opt.eq(&config.reset_liquidity_interval.0) => {
                        match value.parse::<u64>() {
                            Ok(n) => {
                                if n >= 10 {
                                    config.reset_liquidity_interval.1 = n
                                } else {
                                    return Err(anyhow!(
                                        "Error: Number needs to be greater than or equal to 10 for {}.",
                                        config.reset_liquidity_interval.0
                                    ));
                                }
                            }
                            Err(e) => {
                                return Err(anyhow!(
                                    "Error: Could not parse a positive number from `{}` for {}: {}",
                                    value,
                                    config.reset_liquidity_interval.0,
                                    e
                                ))
                            }
                        }
                    }
                    opt if opt.eq(&config.depleteuptopercent.0) => match value.parse::<f64>() {
                        Ok(n) => {
                            if n >= 0.0 && n < 1.0 {
                                config.depleteuptopercent.1 = n
                            } else {
                                return Err(anyhow!(
                                    "Error: Number needs to be between 0 and <1 for {}.",
                                    config.depleteuptopercent.0
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.depleteuptopercent.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.depleteuptoamount.0) => match value.parse::<u64>() {
                        Ok(n) => config.depleteuptoamount.1 = n * 1_000,
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.depleteuptoamount.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.paralleljobs.0) => match value.parse::<u8>() {
                        Ok(n) => {
                            if n > 0 {
                                config.paralleljobs.1 = n
                            } else {
                                return Err(anyhow!(
                                    "Error: Number needs to be greater than 0 for {}.",
                                    config.paralleljobs.0
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.paralleljobs.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.timeoutpay.0) => match value.parse::<u16>() {
                        Ok(n) => {
                            if n > 0 {
                                config.timeoutpay.1 = n
                            } else {
                                return Err(anyhow!(
                                    "Error: Number needs to be greater than 0 for {}.",
                                    config.timeoutpay.0
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.timeoutpay.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.max_htlc_count.0) => match value.parse::<u64>() {
                        Ok(n) => {
                            if n > 0 {
                                config.max_htlc_count.1 = n
                            } else {
                                return Err(anyhow!(
                                    "Error: Number needs to be greater than 0 for {}.",
                                    config.max_htlc_count.0
                                ));
                            }
                        }
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse a positive number from `{}` for {}: {}",
                                value,
                                config.max_htlc_count.0,
                                e
                            ))
                        }
                    },
                    opt if opt.eq(&config.stats_delete_failures_age.0) => {
                        match value.parse::<u64>() {
                            Ok(n) => config.stats_delete_failures_age.1 = n,
                            Err(e) => {
                                return Err(anyhow!(
                                    "Error: Could not parse a number from `{}` for {}: {}",
                                    value,
                                    config.stats_delete_failures_age.0,
                                    e
                                ))
                            }
                        }
                    }
                    opt if opt.eq(&config.stats_delete_failures_size.0) => {
                        match value.parse::<u64>() {
                            Ok(n) => config.stats_delete_failures_size.1 = n,
                            Err(e) => {
                                return Err(anyhow!(
                                    "Error: Could not parse a number from `{}` for {}: {}",
                                    value,
                                    config.stats_delete_failures_size.0,
                                    e
                                ))
                            }
                        }
                    }
                    opt if opt.eq(&config.stats_delete_successes_age.0) => {
                        match value.parse::<u64>() {
                            Ok(n) => config.stats_delete_successes_age.1 = n,
                            Err(e) => {
                                return Err(anyhow!(
                                    "Error: Could not parse a number from `{}` for {}: {}",
                                    value,
                                    config.stats_delete_successes_age.0,
                                    e
                                ))
                            }
                        }
                    }
                    opt if opt.eq(&config.stats_delete_successes_size.0) => {
                        match value.parse::<u64>() {
                            Ok(n) => config.stats_delete_successes_size.1 = n,
                            Err(e) => {
                                return Err(anyhow!(
                                    "Error: Could not parse a number from `{}` for {}: {}",
                                    value,
                                    config.stats_delete_successes_size.0,
                                    e
                                ))
                            }
                        }
                    }
                    opt if opt.eq(&config.channel_health.0) => match value.parse::<bool>() {
                        Ok(b) => config.channel_health.1 = b,
                        Err(e) => {
                            return Err(anyhow!(
                                "Error: Could not parse bool from `{}` for {}: {}",
                                value,
                                config.channel_health.0,
                                e
                            ))
                        }
                    },
                    _ => (),
                }
            }
        }
    }

    Ok(())
}

pub fn get_cli_location(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut config = state.config.lock();
    config.lightning_cli.1 = match plugin.option(&config.lightning_cli.0) {
        Some(options::Value::String(i)) => i,
        Some(_) => config.lightning_cli.1.clone(),
        None => config.lightning_cli.1.clone(),
    };

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
    config.reset_liquidity_interval.1 = match plugin.option(&config.reset_liquidity_interval.0) {
        Some(options::Value::Integer(i)) => {
            if i >= 10 {
                i as u64
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than or equal to 10 and not `{}`.",
                    config.reset_liquidity_interval.0,
                    i
                ));
            }
        }
        Some(_) => config.reset_liquidity_interval.1,
        None => config.reset_liquidity_interval.1,
    };
    config.depleteuptopercent.1 = match plugin.option(&config.depleteuptopercent.0) {
        Some(options::Value::String(i)) => match i.parse::<f64>() {
            Ok(f) => {
                if f >= 0.0 && f < 1.0 {
                    f
                } else {
                    return Err(anyhow!(
                        "Error: {} needs to be greater than 0 and <1, not `{}`.",
                        config.depleteuptopercent.0,
                        f
                    ));
                }
            }
            Err(e) => {
                return Err(anyhow!(
                    "Error: {} could not parse a floating point for `{}`.",
                    e,
                    config.depleteuptopercent.0,
                ))
            }
        },
        Some(_) => config.depleteuptopercent.1,
        None => config.depleteuptopercent.1,
    };
    config.depleteuptoamount.1 = match plugin.option(&config.depleteuptoamount.0) {
        Some(options::Value::Integer(i)) => (i * 1_000) as u64,
        Some(_) => config.depleteuptoamount.1,
        None => config.depleteuptoamount.1,
    };
    config.paralleljobs.1 = match plugin.option(&config.paralleljobs.0) {
        Some(options::Value::Integer(i)) => {
            if i > 0 {
                i as u8
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than 0 and not `{}`.",
                    config.paralleljobs.0,
                    i
                ));
            }
        }
        Some(_) => config.paralleljobs.1,
        None => config.paralleljobs.1,
    };
    config.timeoutpay.1 = match plugin.option(&config.timeoutpay.0) {
        Some(options::Value::Integer(i)) => {
            if i > 0 {
                i as u16
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than 0 and not `{}`.",
                    config.timeoutpay.0,
                    i
                ));
            }
        }
        Some(_) => config.timeoutpay.1,
        None => config.timeoutpay.1,
    };
    config.max_htlc_count.1 = match plugin.option(&config.max_htlc_count.0) {
        Some(options::Value::Integer(i)) => {
            if i > 0 {
                i as u64
            } else {
                return Err(anyhow!(
                    "Error: {} needs to be greater than 0 and not `{}`.",
                    config.max_htlc_count.0,
                    i
                ));
            }
        }
        Some(_) => config.max_htlc_count.1,
        None => config.max_htlc_count.1,
    };
    config.stats_delete_failures_age.1 = match plugin.option(&config.stats_delete_failures_age.0) {
        Some(options::Value::Integer(i)) => i as u64,
        Some(_) => config.stats_delete_failures_age.1,
        None => config.stats_delete_failures_age.1,
    };
    config.stats_delete_failures_size.1 = match plugin.option(&config.stats_delete_failures_size.0)
    {
        Some(options::Value::Integer(i)) => i as u64,
        Some(_) => config.stats_delete_failures_size.1,
        None => config.stats_delete_failures_size.1,
    };
    config.stats_delete_successes_age.1 = match plugin.option(&config.stats_delete_successes_age.0)
    {
        Some(options::Value::Integer(i)) => i as u64,
        Some(_) => config.stats_delete_successes_age.1,
        None => config.stats_delete_successes_age.1,
    };
    config.stats_delete_successes_size.1 =
        match plugin.option(&config.stats_delete_successes_size.0) {
            Some(options::Value::Integer(i)) => i as u64,
            Some(_) => config.stats_delete_successes_size.1,
            None => config.stats_delete_successes_size.1,
        };
    config.channel_health.1 = match plugin.option(&config.channel_health.0) {
        Some(options::Value::Boolean(b)) => b,
        Some(_) => config.channel_health.1,
        None => config.channel_health.1,
    };

    Ok(())
}
