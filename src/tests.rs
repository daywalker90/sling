use cln_rpc::primitives::{PublicKey, ShortChannelId};
use sling::Job;

use crate::dijkstra::dijkstra;
use crate::gossip::read_gossip_file;
use crate::util::feeppm_effective;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::model::{DijkstraNode, ExcludeGraph, IncompleteChannels, LnGraph, PublicKeyPair};

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

        // Signal the sampling thread to stop
        sampling_active.store(false, Ordering::Relaxed);

        // Wait for the sampling thread to finish
        sampling_handle.join().expect("Sampling thread panicked");

        // Calculate peak RAM used
        let peak_rss = peak_rss.load(Ordering::Relaxed);
        println!(
            "Iteration {}: chans:{} nodes:{} incomplete_chans:{} time: {}ms RAM: {}MB",
            i + 1,
            graph.public_channel_count(),
            graph.node_count(),
            incomplete_channels.len(),
            elapsed,
            peak_rss / (1024 * 1024)
        );
        vec_times.push(elapsed);
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
    let rss_avg = vec_rss.iter().sum::<u64>() / iterations as u64;
    println!(
        "average: time {}ms, RAM: {}MB",
        vec_avg,
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

    let start_node =
        PublicKey::from_str("026165850492521f4ac8abd9bd8088123446d126f648ca35e60f88177dc149ceb2")
            .unwrap();

    let slingchan = DijkstraNode {
        score: 0,
        channel_state: graph
            .get_state_no_direction(
                &PublicKey::from_str(
                    "02b3a0d1df18df7121681e994170e8d000c9afc212301249c0fc011f330e141a31",
                )
                .unwrap(),
                &ShortChannelId::from_str("852152x3048x0").unwrap(),
            )
            .unwrap()
            .1,
        short_channel_id: ShortChannelId::from_str("852152x3048x0").unwrap(),
        destination: start_node,
        hops: 0,
    };

    let job = Job {
        sat_direction: sling::SatDirection::Pull,
        amount_msat: 50_000_000,
        outppm: Some(10000),
        maxppm: 5000,
        candidatelist: None,
        target: None,
        maxhops: Some(10),
        depleteuptopercent: None,
        depleteuptoamount: None,
        paralleljobs: Some(1),
        once: None,
        total_amount_msat: None,
    };
    let mut exclude_graph = ExcludeGraph {
        exclude_chans: HashSet::new(),
        exclude_peers: HashSet::new(),
    };
    let two_weeks_ago = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 60 * 60 * 24 * 14) as u32;

    let candidatelist: Vec<ShortChannelId> = graph
        .edges(
            two_weeks_ago,
            &PublicKeyPair {
                my_pubkey: PublicKey::from_str(
                    "0322d0e43b3d92d30ed187f4e101a9a9605c3ee5fc9721e6dac3ce3d7732fbb13e",
                )
                .unwrap(),
                other_pubkey: start_node,
            },
            &exclude_graph,
            &job,
            &[],
            &HashMap::new(),
            &[],
            &HashMap::new(),
        )
        .iter()
        .map(|r| r.0.short_channel_id)
        .collect();

    for _i in 0..iterations {
        let now = Instant::now();
        let route = dijkstra(
            &start_node,
            &graph,
            &start_node,
            &PublicKey::from_str(
                "02b3a0d1df18df7121681e994170e8d000c9afc212301249c0fc011f330e141a31",
            )
            .unwrap(),
            &slingchan,
            &job,
            &candidatelist,
            10,
            &exclude_graph,
            80,
            &HashMap::new(),
            &[],
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
        exclude_graph
            .exclude_chans
            .insert(route.first().unwrap().channel);
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
