use cln_rpc::model::responses::GetinfoResponse;
use cln_rpc::primitives::{Amount, PublicKey, ShortChannelId, ShortChannelIdDir};
use sling::Job;

use crate::dijkstra::dijkstra;
use crate::gossip::read_gossip_file;
use crate::util::feeppm_effective;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::model::{Config, IncompleteChannels, JobMessage, LnGraph, Task, PLUGIN_NAME};

#[test]
fn test_effective_feeppm() {
    assert_eq!(feeppm_effective(0, 0, 1_000), 0);
    assert_eq!(feeppm_effective(0, 0, 200_000), 0);
    assert_eq!(feeppm_effective(0, 0, 9_999_999_999), 0);

    assert_eq!(feeppm_effective(1, 0, 1_000), 1);
    assert_eq!(feeppm_effective(68, 0, 1_000), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 1_000), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 1_000), 1_000);
    assert_eq!(feeppm_effective(0, 53, 1_000), 53_000);
    assert_eq!(feeppm_effective(0, 10_009_000, 1_000), 10_009_000_000);

    assert_eq!(feeppm_effective(1, 0, 200_000), 1);
    assert_eq!(feeppm_effective(68, 0, 200_000), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 200_000), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 200_000), 5);
    assert_eq!(feeppm_effective(0, 53, 200_000), 265);
    assert_eq!(feeppm_effective(0, 10_009_000, 200_000), 50_045_000);

    assert_eq!(feeppm_effective(100, 0, 9_999_999_999), 100);
    assert_eq!(feeppm_effective(68, 0, 9_999_999_999), 68);
    assert_eq!(feeppm_effective(11_115_555, 0, 9_999_999_999), 11_115_555);

    assert_eq!(feeppm_effective(0, 1, 9_999_999_999), 1);
    assert_eq!(feeppm_effective(0, 53, 9_999_999_999), 1);
    assert_eq!(feeppm_effective(0, 10_009_000, 9_999_999_999), 1_001);

    assert_eq!(feeppm_effective(1, 1, 1_000), 1_001);
    assert_eq!(feeppm_effective(68, 53, 1_000), 53_068);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 1_000),
        10_020_115_555
    );

    assert_eq!(feeppm_effective(1, 1, 200_000), 6);
    assert_eq!(feeppm_effective(68, 53, 200_000), 333);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 200_000),
        61_160_555
    );

    assert_eq!(feeppm_effective(1, 1, 9_999_999_999), 2);
    assert_eq!(feeppm_effective(68, 53, 9_999_999_999), 69);
    assert_eq!(
        feeppm_effective(11_115_555, 10_009_000, 9_999_999_999),
        11_116_556
    );

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 1_000),
        4_299_262_262_296
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 1_000), 4_294_967_295_000);
    assert_eq!(feeppm_effective(u32::MAX, 0, 1_000), 4_294_967_296);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 200_000),
        25_769_803_770
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 200_000), 21_474_836_475);
    assert_eq!(feeppm_effective(u32::MAX, 0, 200_000), 4_294_967_296);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, 9_999_999_999),
        4_295_396_792
    );
    assert_eq!(feeppm_effective(0, u32::MAX, 9_999_999_999), 429_497);
    assert_eq!(feeppm_effective(u32::MAX, 0, 9_999_999_999), 4_294_967_296);

    assert_eq!(feeppm_effective(0, 0, u64::MAX), 0);

    assert_eq!(
        feeppm_effective(u32::MAX, u32::MAX, u64::MAX),
        4_294_967_296
    );
    assert_eq!(feeppm_effective(0, u32::MAX, u64::MAX), 1);
    assert_eq!(feeppm_effective(u32::MAX, 0, u64::MAX), 4_294_967_296);
}

#[test]
fn test_fee_total() {
    use crate::util::fee_total_msat_precise;
    assert_eq!(fee_total_msat_precise(0, 0, 1_000).ceil() as u64, 0);

    assert_eq!(fee_total_msat_precise(1, 0, 1_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(349, 0, 1_000).ceil() as u64, 1);
    assert_eq!(
        fee_total_msat_precise(u32::MAX, 0, 1_000).ceil() as u64,
        4_294_968
    );

    assert_eq!(fee_total_msat_precise(0, 1, 1_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(0, 1234, 1_000).ceil() as u64, 1234);
    assert_eq!(
        fee_total_msat_precise(0, u32::MAX, 1_000).ceil() as u64,
        4_294_967_295
    );

    assert_eq!(fee_total_msat_precise(1, 1, 1_000).ceil() as u64, 2);
    assert_eq!(
        fee_total_msat_precise(349, 1234, 1_000).ceil() as u64,
        1_235
    );
    assert_eq!(
        fee_total_msat_precise(u32::MAX, u32::MAX, 1_000).ceil() as u64,
        4_299_262_263
    );

    assert_eq!(fee_total_msat_precise(0, 0, 200_000).ceil() as u64, 0);

    assert_eq!(fee_total_msat_precise(1, 0, 200_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(349, 0, 200_000).ceil() as u64, 70);
    assert_eq!(
        fee_total_msat_precise(u32::MAX, 0, 200_000).ceil() as u64,
        858_993_460
    );

    assert_eq!(fee_total_msat_precise(0, 1, 200_000).ceil() as u64, 1);
    assert_eq!(fee_total_msat_precise(0, 1234, 200_000).ceil() as u64, 1234);
    assert_eq!(
        fee_total_msat_precise(0, u32::MAX, 200_000).ceil() as u64,
        4_294_967_295
    );

    assert_eq!(fee_total_msat_precise(1, 1, 200_000).ceil() as u64, 2);
    assert_eq!(
        fee_total_msat_precise(349, 1234, 200_000).ceil() as u64,
        1_304
    );
    assert_eq!(
        fee_total_msat_precise(u32::MAX, u32::MAX, 200_000).ceil() as u64,
        5_153_960_754
    );
}

#[test]
fn test_feeppm_effective_from_amts() {
    use crate::util::feeppm_effective_from_amts;
    assert_eq!(feeppm_effective_from_amts(1_000, 1_000), 0);
    assert_eq!(feeppm_effective_from_amts(u64::MAX, u64::MAX), 0);

    assert_eq!(feeppm_effective_from_amts(200_000_200, 200_000_000), 1);
    assert_eq!(feeppm_effective_from_amts(201_001_234, 201_000_000), 7);
    assert_eq!(feeppm_effective_from_amts(100_125_200, 100_000_000), 1_252);

    let result1 = std::panic::catch_unwind(|| feeppm_effective_from_amts(1_000, 2_000));
    assert!(result1.is_err());
}

// Read current RSS from /proc/self/statm (Linux-specific)
fn get_current_rss() -> Option<u64> {
    let mut file = File::open("/proc/self/statm").ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    let fields: Vec<&str> = contents.split_whitespace().collect();
    let resident_pages: u64 = fields.get(1)?.parse().ok()?;
    Some(resident_pages * 4096) // Convert pages to bytes (4KB page size)
}
#[test]
fn test_gossip_file_reader() {
    let iterations = 5;
    let mut vec_times = Vec::new();
    let mut vec_after_times = Vec::new();
    let mut vec_rss = Vec::new();

    for i in 0..iterations {
        let peak_rss = Arc::new(AtomicU64::new(0));
        let sampling_active = Arc::new(std::sync::atomic::AtomicBool::new(true));

        // Spawn a thread to sample RSS
        let peak_rss_clone = Arc::clone(&peak_rss);
        let sampling_active_clone = Arc::clone(&sampling_active);
        let sampling_handle = thread::spawn(move || {
            while sampling_active_clone.load(Ordering::Relaxed) {
                if let Some(rss) = get_current_rss() {
                    peak_rss_clone.fetch_max(rss, Ordering::Relaxed);
                }
                thread::sleep(Duration::from_millis(1)); // Sample every 1ms
            }
        });

        let gossip_file = File::open(Path::new("gossip_store")).expect("Failed to open file");
        let mut reader = BufReader::new(gossip_file);
        let mut is_start_up = true;
        let mut offset = 0;
        let mut graph = LnGraph::new();
        let mut incomplete_channels = IncompleteChannels::new();

        let now = Instant::now();
        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete_channels,
            &mut offset,
        )
        .expect("read_gossip_file failed");
        let elapsed = now.elapsed().as_millis();
        vec_times.push(elapsed);

        let now = Instant::now();
        read_gossip_file(
            &mut is_start_up,
            &mut reader,
            &mut graph,
            &mut incomplete_channels,
            &mut offset,
        )
        .expect("read_gossip_file failed");
        let elapsed_after = now.elapsed().as_millis();
        vec_after_times.push(elapsed_after);

        // Signal the sampling thread to stop
        sampling_active.store(false, Ordering::Relaxed);

        // Wait for the sampling thread to finish
        sampling_handle.join().expect("Sampling thread panicked");

        // Calculate peak RAM used
        let peak_rss = peak_rss.load(Ordering::Relaxed);
        println!(
            "Iteration {}: chans:{} nodes:{} incomplete_chans:{} time: {}ms after: {}ms RAM: {}MB",
            i + 1,
            graph.public_channel_count(),
            graph.node_count(),
            incomplete_channels.len(),
            elapsed,
            elapsed_after,
            peak_rss / (1024 * 1024)
        );
        vec_rss.push(peak_rss);
        File::create(Path::new("graph_refactor.json"))
            .unwrap()
            .write_all(&serde_json::to_vec_pretty(&graph).unwrap())
            .unwrap();
        File::create(Path::new("incomplete_graph_refactor.json"))
            .unwrap()
            .write_all(&serde_json::to_vec_pretty(&incomplete_channels).unwrap())
            .unwrap();
    }
    let vec_avg = vec_times.iter().sum::<u128>() / iterations as u128;
    let vec_after_avg = vec_after_times.iter().sum::<u128>() / iterations as u128;
    let rss_avg = vec_rss.iter().sum::<u64>() / iterations as u64;
    println!(
        "average: time {}ms, after {}ms, RAM: {}MB",
        vec_avg,
        vec_after_avg,
        rss_avg / (1024 * 1024)
    );
}

#[test]
fn test_dijkstra_speed() {
    let iterations = 100;
    let mut vec_times = Vec::new();
    let mut vec_len = Vec::new();

    let peak_rss = Arc::new(AtomicU64::new(0));
    let sampling_active = Arc::new(std::sync::atomic::AtomicBool::new(true));

    let peak_rss_clone = Arc::clone(&peak_rss);
    let sampling_active_clone = Arc::clone(&sampling_active);
    let sampling_handle = thread::spawn(move || {
        while sampling_active_clone.load(Ordering::Relaxed) {
            if let Some(rss) = get_current_rss() {
                peak_rss_clone.fetch_max(rss, Ordering::Relaxed);
            }
            thread::sleep(Duration::from_millis(1)); // Sample every 1ms
        }
    });

    let gossip_file = File::open(Path::new("gossip_store")).expect("Failed to open file");
    let mut reader = BufReader::new(gossip_file);
    let mut is_start_up = true;
    let mut offset = 0;
    let mut graph = LnGraph::new();
    let mut incomplete_channels = IncompleteChannels::new();
    read_gossip_file(
        &mut is_start_up,
        &mut reader,
        &mut graph,
        &mut incomplete_channels,
        &mut offset,
    )
    .expect("read_gossip_file failed");

    let start_node = crate::model::PubKeyBytes::from_str(
        "026165850492521f4ac8abd9bd8088123446d126f648ca35e60f88177dc149ceb2",
    )
    .unwrap();
    let rando_node =
        PublicKey::from_str("0221299f99b760f35851a3a6ae60f62680b79ec02a34c3b25f6ba1e0ce9da62ba7")
            .unwrap();

    let job = Job::new(sling::SatDirection::Pull, 50_000_000, Some(10000), 5000);
    let mut task = Task::new(
        ShortChannelId::from_str("852152x3048x0").unwrap(),
        1,
        JobMessage::Starting,
        false,
        crate::model::PubKeyBytes::from_str(
            "02b3a0d1df18df7121681e994170e8d000c9afc212301249c0fc011f330e141a31",
        )
        .unwrap(),
    );
    let two_weeks_ago = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 60 * 60 * 24 * 14) as u32;

    let getinfo = GetinfoResponse {
        lightning_dir: String::new(),
        alias: None,
        our_features: None,
        warning_bitcoind_sync: None,
        warning_lightningd_sync: None,
        address: None,
        binding: None,
        blockheight: 1,
        color: String::new(),
        fees_collected_msat: Amount::from_msat(0),
        id: rando_node,
        network: "regtest".to_owned(),
        num_active_channels: 1,
        num_inactive_channels: 0,
        num_peers: 1,
        num_pending_channels: 0,
        version: "v25.02".to_owned(),
    };

    let mut config = Config::new(
        getinfo,
        PathBuf::from("lightning-rpc"),
        PathBuf::from(PLUGIN_NAME),
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    );

    let candidatelist: Vec<ShortChannelId> = graph
        .edges(
            &start_node,
            two_weeks_ago,
            &[],
            &config,
            &job,
            &[],
            &HashMap::new(),
        )
        .iter()
        .map(|r| r.0.short_channel_id)
        .collect();
    println!("candidates: {}", candidatelist.len());
    config.pubkey_bytes = start_node;
    // task.actual_candidates = candidatelist;

    let mut excepts: Vec<ShortChannelIdDir> = Vec::new();

    println!("ready for profiling");
    sleep(Duration::from_secs(4));

    for _i in 0..iterations {
        let now = Instant::now();
        let route = dijkstra(
            &config,
            &graph,
            &job,
            &mut task,
            &candidatelist,
            &excepts,
            &HashMap::new(),
        )
        .unwrap();
        let elapsed = now.elapsed().as_millis();
        // for r in &route {
        //     println!(
        //         "{}/{}: route: {} {:4} {:17}",
        //         slingchan.short_channel_id,
        //         1,
        //         Amount::msat(&r.amount_msat),
        //         r.delay,
        //         r.channel.to_string(),
        //     );
        // }
        // println!("----------------------------------");
        if let Some(hop) = route.first() {
            excepts.push(ShortChannelIdDir {
                short_channel_id: hop.channel,
                direction: 0,
            });
            excepts.push(ShortChannelIdDir {
                short_channel_id: hop.channel,
                direction: 1,
            });
        }
        vec_len.push(route.len());
        vec_times.push(elapsed);
    }

    let vec_avg = vec_times.iter().sum::<u128>() / iterations as u128;
    let len_avg = vec_len.iter().sum::<usize>() / iterations as usize;

    // Signal the sampling thread to stop
    sampling_active.store(false, Ordering::Relaxed);

    // Wait for the sampling thread to finish
    sampling_handle.join().expect("Sampling thread panicked");

    // Calculate peak RAM used
    let peak_rss = peak_rss.load(Ordering::Relaxed);

    println!(
        "average: route_len:{} time {}ms, peak RAM: {}MB",
        len_avg,
        vec_avg,
        peak_rss / (1024 * 1024)
    );
}
