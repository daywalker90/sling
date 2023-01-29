use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    list_channels, list_nodes, list_peers, make_rpc_path,
    model::{DirectedChannel, LnGraph},
    util::{read_graph, read_jobs, write_graph},
    PluginState, PLUGIN_NAME,
};
use anyhow::Error;
use cln_plugin::Plugin;
use cln_rpc::primitives::Amount;

use log::info;

use tokio::time::{self, Instant};

pub async fn refresh_aliasmap(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path = make_rpc_path(&plugin);
    loop {
        let now = Instant::now();
        {
            let nodes = list_nodes(&rpc_path, None).await?.nodes;
            *plugin.state().alias_peer_map.lock() = nodes
                .into_iter()
                .filter_map(|node| node.alias.map(|alias| (node.nodeid, alias)))
                .collect();
        }
        info!(
            "Refreshing alias map done in {}ms!",
            now.elapsed().as_millis().to_string()
        );
        time::sleep(Duration::from_secs(3600)).await;
    }
}

pub async fn refresh_listpeers(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path = make_rpc_path(&plugin);

    loop {
        {
            let now = Instant::now();
            *plugin.state().peers.lock() = list_peers(&rpc_path).await?.peers;
            info!(
                "Peers refreshed in {}ms",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(5)).await;
    }
}

pub async fn refresh_graph(plugin: Plugin<PluginState>) -> Result<(), Error> {
    let rpc_path = make_rpc_path(&plugin);
    let sling_dir = Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME);
    *plugin.state().graph.lock() = read_graph(&sling_dir).await?;

    loop {
        {
            let now = Instant::now();
            info!("Getting all channels in gossip...");
            let channels = list_channels(&rpc_path, None, None, None).await?.channels;
            info!(
                "Getting all channels done in {}s!",
                now.elapsed().as_secs().to_string()
            );
            let peers = plugin.state().peers.lock().clone();
            let jobs = read_jobs(
                &Path::new(&plugin.configuration().lightning_dir).join(PLUGIN_NAME),
                &peers,
            )
            .await?;

            let amounts = jobs.values().map(|job| job.amount);
            let min_amount = amounts.clone().min().unwrap_or(1_000);
            let max_amount = amounts.max().unwrap_or(10_000_000_000);
            let maxppms = jobs.values().map(|job| job.maxppm);
            // let min_maxppm = maxppms.min().unwrap_or(0);
            let max_maxppm = maxppms.max().unwrap_or(3_000);

            let mypubkey = plugin.state().config.lock().pubkey.unwrap().clone();

            plugin.state().graph.lock().update(LnGraph {
                graph: channels
                    .into_iter()
                    .filter(|chan| {
                        (chan.active
                            && chan.delay <= 288
                            && Amount::msat(&chan.htlc_maximum_msat.unwrap_or(chan.amount_msat))
                                >= min_amount
                            && Amount::msat(&chan.htlc_minimum_msat) <= max_amount
                            && chan.fee_per_millionth <= max_maxppm)
                            || chan.source == mypubkey
                            || chan.destination == mypubkey
                    })
                    .fold(HashMap::new(), |mut map, chan| {
                        map.entry(chan.source)
                            .or_insert_with(Vec::new)
                            .push(DirectedChannel::new(chan));
                        map
                    }),
            });

            write_graph(plugin.clone()).await?;
            info!(
                "Built and saved graph in {}ms!",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(600)).await;
    }
}

pub async fn refresh_liquidity(plugin: Plugin<PluginState>) -> Result<(), Error> {
    loop {
        {
            let now = Instant::now();
            plugin.state().graph.lock().refresh_liquidity();
            info!(
                "Refreshed Liquidity in {}ms!",
                now.elapsed().as_millis().to_string()
            );
        }
        time::sleep(Duration::from_secs(900)).await;
    }
}

pub async fn clear_tempbans(plugin: Plugin<PluginState>) -> Result<(), Error> {
    loop {
        {
            plugin.state().tempbans.lock().retain(|_c, t| {
                *t > SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    - 600
            })
        }
        time::sleep(Duration::from_secs(100)).await;
    }
}
