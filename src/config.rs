use anyhow::{anyhow, Error};
use chrono::Utc;
use cln_plugin::{options, ConfiguredPlugin};
use log::{info, warn};
use std::path::Path;

use tokio::fs;

use crate::{
    model::PluginState,
    rpc::{get_config_path, get_info},
};

fn validate_u64_input(
    value: u64,
    var_name: &String,
    gteq: u64,
    time_factor_to_secs: Option<u64>,
) -> Result<u64, Error> {
    if value < gteq {
        return Err(anyhow!(
            "{} must be greater than or equal to {}",
            var_name,
            gteq
        ));
    }

    if let Some(factor) = time_factor_to_secs {
        if !is_valid_hour_timestamp(value.saturating_mul(factor)) {
            return Err(anyhow!(
                "{} needs to be a positive number and smaller than {}, \
            not `{}`.",
                var_name,
                (Utc::now().timestamp() as u64),
                value
            ));
        }
    }

    Ok(value)
}

fn is_valid_hour_timestamp(val: u64) -> bool {
    Utc::now().timestamp() as u64 > val
}

fn options_value_to_u64(
    config_var: &(String, u64),
    value: Option<options::Value>,
    gteq: u64,
    time_factor_to_secs: Option<u64>,
) -> Result<u64, Error> {
    match value {
        Some(options::Value::Integer(i)) => {
            if i >= 0 {
                validate_u64_input(i as u64, &config_var.0, gteq, time_factor_to_secs)
            } else {
                Err(anyhow!(
                    "{} needs to be a positive number and not `{}`.",
                    config_var.0,
                    i
                ))
            }
        }
        Some(_) => Ok(config_var.1),
        None => Ok(config_var.1),
    }
}

fn str_to_u64(
    var_name: &String,
    value: &str,
    gteq: u64,
    time_factor_to_secs: Option<u64>,
) -> Result<u64, Error> {
    match value.parse::<u64>() {
        Ok(n) => validate_u64_input(n, var_name, gteq, time_factor_to_secs),
        Err(e) => Err(anyhow!(
            "Could not parse a positive number from `{}` for {}: {}",
            value,
            var_name,
            e
        )),
    }
}

pub async fn read_config(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut config_file_content = String::new();
    let dir = plugin.configuration().lightning_dir;
    let rpc_path = Path::new(&dir).join(plugin.configuration().rpc_file);
    let getinfo = get_info(&rpc_path).await?;
    let config = state.config.lock().clone();
    let config_file_path = if config.lightning_conf.1.is_empty() {
        get_config_path(getinfo.lightning_dir).await?
    } else {
        vec![config.lightning_conf.1]
    };

    for confs in &config_file_path {
        match fs::read_to_string(Path::new(&confs)).await {
            Ok(f) => {
                info!("Found config file: {}", confs);
                config_file_content += &(f + "\n")
            }
            Err(e) => info!("Not a config file {}! {}", confs, e.to_string()),
        }
    }

    if config_file_content.is_empty() {
        warn!(
            "No config file found! Searched here: {}",
            config_file_path.join(", ")
        );
    }

    let mut config = state.config.lock();

    for line in config_file_content.lines() {
        if line.contains('=') {
            let splitline = line.split('=').collect::<Vec<&str>>();
            if splitline.len() == 2 {
                let name = splitline.first().unwrap();
                let value = splitline.get(1).unwrap();

                match name {
                    opt if opt.eq(&config.cltv_delta.0) => {
                        config.cltv_delta.1 =
                            Some(str_to_u64(&config.cltv_delta.0, value, 0, None)? as u16)
                    }
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
                    opt if opt.eq(&config.refresh_peers_interval.0) => {
                        config.refresh_peers_interval.1 =
                            str_to_u64(&config.refresh_peers_interval.0, value, 1, None)?
                    }
                    opt if opt.eq(&config.refresh_aliasmap_interval.0) => {
                        config.refresh_aliasmap_interval.1 =
                            str_to_u64(&config.refresh_aliasmap_interval.0, value, 1, None)?
                    }
                    opt if opt.eq(&config.refresh_graph_interval.0) => {
                        config.refresh_graph_interval.1 =
                            str_to_u64(&config.refresh_graph_interval.0, value, 1, None)?
                    }
                    opt if opt.eq(&config.reset_liquidity_interval.0) => {
                        config.reset_liquidity_interval.1 =
                            str_to_u64(&config.reset_liquidity_interval.0, value, 10, None)?
                    }
                    opt if opt.eq(&config.depleteuptopercent.0) => match value.parse::<f64>() {
                        Ok(n) => {
                            if (0.0..1.0).contains(&n) {
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
                    opt if opt.eq(&config.depleteuptoamount.0) => {
                        config.depleteuptoamount.1 =
                            str_to_u64(&config.depleteuptoamount.0, value, 0, None)? * 1000
                    }
                    opt if opt.eq(&config.maxhops.0) => {
                        config.maxhops.1 = str_to_u64(&config.maxhops.0, value, 2, None)? as u8
                    }
                    opt if opt.eq(&config.candidates_min_age.0) => {
                        config.candidates_min_age.1 =
                            str_to_u64(&config.candidates_min_age.0, value, 0, None)? as u32
                    }
                    opt if opt.eq(&config.paralleljobs.0) => {
                        config.paralleljobs.1 =
                            str_to_u64(&config.paralleljobs.0, value, 1, None)? as u8
                    }
                    opt if opt.eq(&config.timeoutpay.0) => {
                        config.timeoutpay.1 =
                            str_to_u64(&config.timeoutpay.0, value, 1, None)? as u16
                    }
                    opt if opt.eq(&config.max_htlc_count.0) => {
                        config.max_htlc_count.1 =
                            str_to_u64(&config.max_htlc_count.0, value, 1, None)?
                    }
                    opt if opt.eq(&config.stats_delete_failures_age.0) => {
                        config.stats_delete_failures_age.1 = str_to_u64(
                            &config.stats_delete_failures_age.0,
                            value,
                            0,
                            Some(24 * 60 * 60),
                        )?
                    }
                    opt if opt.eq(&config.stats_delete_failures_size.0) => {
                        config.stats_delete_failures_size.1 =
                            str_to_u64(&config.stats_delete_failures_size.0, value, 0, None)?
                    }
                    opt if opt.eq(&config.stats_delete_successes_age.0) => {
                        config.stats_delete_successes_age.1 = str_to_u64(
                            &config.stats_delete_successes_age.0,
                            value,
                            0,
                            Some(24 * 60 * 60),
                        )?
                    }
                    opt if opt.eq(&config.stats_delete_successes_size.0) => {
                        config.stats_delete_successes_size.1 =
                            str_to_u64(&config.stats_delete_successes_size.0, value, 0, None)?
                    }
                    _ => (),
                }
            }
        }
    }

    Ok(())
}

pub fn get_prestart_configs(
    plugin: &ConfiguredPlugin<PluginState, tokio::io::Stdin, tokio::io::Stdout>,
    state: PluginState,
) -> Result<(), Error> {
    let mut config = state.config.lock();
    config.lightning_conf.1 = match plugin.option(&config.lightning_conf.0) {
        Some(options::Value::String(i)) => i,
        Some(_) => config.lightning_conf.1.clone(),
        None => config.lightning_conf.1.clone(),
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
    config.refresh_peers_interval.1 = options_value_to_u64(
        &config.refresh_peers_interval,
        plugin.option(&config.refresh_peers_interval.0),
        1,
        None,
    )?;
    config.refresh_aliasmap_interval.1 = options_value_to_u64(
        &config.refresh_aliasmap_interval,
        plugin.option(&config.refresh_aliasmap_interval.0),
        1,
        None,
    )?;
    config.refresh_graph_interval.1 = options_value_to_u64(
        &config.refresh_graph_interval,
        plugin.option(&config.refresh_graph_interval.0),
        1,
        None,
    )?;
    config.reset_liquidity_interval.1 = options_value_to_u64(
        &config.reset_liquidity_interval,
        plugin.option(&config.reset_liquidity_interval.0),
        10,
        None,
    )?;
    config.depleteuptopercent.1 = match plugin.option(&config.depleteuptopercent.0) {
        Some(options::Value::String(i)) => match i.parse::<f64>() {
            Ok(f) => {
                if (0.0..1.0).contains(&f) {
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
    config.depleteuptoamount.1 = options_value_to_u64(
        &config.depleteuptoamount,
        plugin.option(&config.depleteuptoamount.0),
        0,
        None,
    )? * 1000;
    config.maxhops.1 = options_value_to_u64(
        &(config.maxhops.0.clone(), config.maxhops.1 as u64),
        plugin.option(&config.maxhops.0),
        2,
        None,
    )? as u8;
    config.candidates_min_age.1 = options_value_to_u64(
        &(
            config.candidates_min_age.0.clone(),
            config.candidates_min_age.1 as u64,
        ),
        plugin.option(&config.candidates_min_age.0),
        0,
        None,
    )? as u32;
    config.paralleljobs.1 = options_value_to_u64(
        &(config.paralleljobs.0.clone(), config.paralleljobs.1 as u64),
        plugin.option(&config.paralleljobs.0),
        1,
        None,
    )? as u8;
    config.timeoutpay.1 = options_value_to_u64(
        &(config.timeoutpay.0.clone(), config.timeoutpay.1 as u64),
        plugin.option(&config.timeoutpay.0),
        1,
        None,
    )? as u16;
    config.max_htlc_count.1 = options_value_to_u64(
        &config.max_htlc_count,
        plugin.option(&config.max_htlc_count.0),
        1,
        None,
    )?;
    config.stats_delete_failures_age.1 = options_value_to_u64(
        &config.stats_delete_failures_age,
        plugin.option(&config.stats_delete_failures_age.0),
        0,
        Some(24 * 60 * 60),
    )?;
    config.stats_delete_failures_size.1 = options_value_to_u64(
        &config.stats_delete_failures_size,
        plugin.option(&config.stats_delete_failures_size.0),
        0,
        None,
    )?;
    config.stats_delete_successes_age.1 = options_value_to_u64(
        &config.stats_delete_successes_age,
        plugin.option(&config.stats_delete_successes_age.0),
        0,
        Some(24 * 60 * 60),
    )?;
    config.stats_delete_successes_size.1 = options_value_to_u64(
        &config.stats_delete_successes_size,
        plugin.option(&config.stats_delete_successes_size.0),
        0,
        None,
    )?;

    Ok(())
}
