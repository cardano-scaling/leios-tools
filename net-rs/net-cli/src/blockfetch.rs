use net_core::mux::scheduler::TrafficClass;
use net_core::mux::ProtocolConfig;
use net_core::protocols::blockfetch;
use net_core::protocols::blockfetch::BlockFetch;
use net_core::protocols::chainsync;
use net_core::protocols::chainsync::ChainSync;
use net_core::protocols::Role;
use net_core::protocols::Runner;
use net_core::types::Point;

use crate::connect;

pub async fn run(
    host: &str,
    magic: u64,
    point: Option<Point>,
    dump_hex: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cs_proto = ProtocolConfig {
        id: chainsync::PROTOCOL_ID,
        traffic_class: TrafficClass::Priority,
        ingress_limit: chainsync::INGRESS_LIMIT,
        egress_queue_size: 16,
    };
    let bf_proto = ProtocolConfig {
        id: blockfetch::PROTOCOL_ID,
        traffic_class: TrafficClass::Priority,
        ingress_limit: blockfetch::INGRESS_LIMIT,
        egress_queue_size: 16,
    };

    println!("connecting to {host}...");
    let conn = connect::connect_and_handshake(host, magic, &[cs_proto, bf_proto]).await?;
    let running = conn.running;

    let mut channels = conn.channels.into_iter();
    let (cs_send, cs_recv) = channels.next().expect("chainsync channel");
    let (bf_send, bf_recv) = channels.next().expect("blockfetch channel");

    let mut cs_runner = Runner::<ChainSync>::new(Role::Client, cs_send, cs_recv);

    let fetch_point = match point {
        Some(p) => {
            println!("fetching at supplied point {p}...");
            p
        }
        None => {
            // ChainSync: find the tip so we have a point for BlockFetch.
            println!("finding intersection...");
            let result =
                chainsync::find_intersection(&mut cs_runner, vec![Point::Origin]).await?;
            let tip = match result {
                Some((_point, tip)) => {
                    println!("intersection found, tip: {tip}");
                    tip
                }
                None => {
                    println!("no intersection found");
                    running.abort();
                    return Ok(());
                }
            };

            let fetch_point = tip.point.clone();
            if fetch_point == Point::Origin {
                println!("tip is origin, nothing to fetch");
                chainsync::done(&mut cs_runner).await?;
                running.abort();
                return Ok(());
            }
            fetch_point
        }
    };

    println!("fetching block at {fetch_point}...");

    let mut bf_runner = Runner::<BlockFetch>::new(Role::Client, bf_send, bf_recv);

    let has_blocks =
        blockfetch::request_range(&mut bf_runner, fetch_point.clone(), fetch_point).await?;

    if has_blocks {
        let mut block_count = 0u64;
        while let Some(body) = blockfetch::recv_block(&mut bf_runner).await? {
            block_count += 1;
            println!("  block #{block_count}: {} bytes", body.raw.len());
            if dump_hex {
                use std::fmt::Write as _;
                let mut s = String::with_capacity(body.raw.len() * 2);
                for b in &body.raw {
                    let _ = write!(s, "{b:02x}");
                }
                println!("  hex: {s}");
            }
        }
        println!("batch complete: {block_count} block(s)");
    } else {
        println!("server has no blocks for requested range");
    }

    // Clean shutdown.
    blockfetch::done(&mut bf_runner).await?;
    chainsync::done(&mut cs_runner).await?;
    running.abort();
    println!("done");
    Ok(())
}
