//! Server-side protocol handlers for responder peer tasks.
//!
//! Each handler runs one mini-protocol in server (responder) mode,
//! reading from an `Arc<ChainStore>` for chain state and sending
//! events to the coordinator via an `mpsc` channel.

use std::sync::Arc;

use tokio::sync::mpsc;

use shared_consensus::behaviour::{BehaviourHandle, Outbound, OutboundDecision, OwnedOutbound};

use crate::types::{BlockBody, Point, Tip, WrappedHeader};

use crate::mux::{CodecRecv, CodecSend};
use crate::protocols::blockfetch::{BlockFetch, Message as BfMsg};
use crate::protocols::chainsync::{ChainSync, Message as CsMsg};
use crate::protocols::keepalive::{KeepAlive, Message as KaMsg};
use crate::protocols::leios_fetch::{LeiosFetch, Message as LfMsg};
use crate::protocols::leios_notify::{LeiosNotify, Message as LnMsg};
use crate::protocols::peersharing::{Message as PsMsg, PeerAddress, PeerSharing};
use crate::protocols::txsubmission::{self, Message as TsMsg, TxSubmission};
use crate::protocols::{Role, Runner};

use super::types::PeerEvent;
use super::PeerId;
use crate::store::chain_store::ChainStore;
use crate::store::leios_store::LeiosStore;

/// Serve ChainSync for one connection.
///
/// Responds to intersection queries and streams headers as the chain
/// advances. Uses `ChainStore::subscribe()` to wake when new blocks arrive.
///
/// When `behaviour` is `Some`, every `MsgRollForward` is filtered
/// through
/// [`Behaviour::transform_outbound`](shared_consensus::behaviour::Behaviour::transform_outbound)
/// before going on the wire — the behaviour can substitute a different
/// header for this `peer` (peer-split equivocation), suppress the send
/// (partition / eclipse mute), or augment with extras.
pub async fn serve_chainsync(
    cs_send: CodecSend,
    cs_recv: CodecRecv,
    store: Arc<ChainStore>,
    peer: PeerId,
    behaviour: Option<BehaviourHandle>,
) {
    let mut runner = Runner::<ChainSync>::new(Role::Server, cs_send, cs_recv);
    let mut read_index: Option<usize> = None;
    let mut read_point: Option<Point> = None;
    let mut subscription = store.subscribe();
    let mut first_recv = true;

    loop {
        let msg = match runner.recv().await {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    peer = peer.0,
                    err = %e,
                    "chainsync: serve_chainsync recv error, exiting"
                );
                break;
            }
        };
        if first_recv {
            tracing::info!(peer = peer.0, "chainsync: downstream opened protocol with us");
            first_recv = false;
        }

        match msg {
            CsMsg::MsgFindIntersect { points } => {
                tracing::info!(
                    peer = peer.0,
                    candidates = points.len(),
                    "chainsync: downstream sent MsgFindIntersect (wants to sync chain from us)"
                );
                match store.find_intersection(&points) {
                    Some((point, tip)) => {
                        read_index = store.index_of(&point);
                        read_point = Some(point.clone());
                        let _ = runner.send(&CsMsg::MsgIntersectFound { point, tip }).await;
                    }
                    None => {
                        let tip = store.tip();
                        let _ = runner.send(&CsMsg::MsgIntersectNotFound { tip }).await;
                    }
                }
            }
            CsMsg::MsgRequestNext => {
                // Check if our read pointer was invalidated by a rollback.
                // Point-matching detects the case where a rollback + re-append
                // leaves the same index occupied by a different block.
                if !store.is_valid_index(read_index, &read_point) {
                    // Roll back to the fork point (where the chain was truncated).
                    let rollback_target = store.last_rollback_target().unwrap_or(Point::Origin);
                    let tip = store.tip();
                    read_index = store.index_of(&rollback_target);
                    read_point = Some(rollback_target.clone());
                    let _ = runner
                        .send(&CsMsg::MsgRollBackward {
                            point: rollback_target,
                            tip,
                        })
                        .await;
                } else {
                    let pending = store.blocks_after(read_index);
                    if let Some(block) = pending.first() {
                        read_index = store.index_of(&block.point);
                        read_point = Some(block.point.clone());
                        let tip = store.tip();
                        send_roll_forward(
                            &mut runner,
                            block.header.clone(),
                            &block.point,
                            tip,
                            peer,
                            behaviour.as_ref(),
                        )
                        .await;
                    } else {
                        tracing::info!(
                            peer = peer.0,
                            "chainsync: sending MsgAwaitReply (entering StMustReply — no chain past downstream's intersection)"
                        );
                        let _ = runner.send(&CsMsg::MsgAwaitReply).await;

                        loop {
                            if subscription.changed().await.is_err() {
                                return;
                            }
                            // After waking, check for rollback first.
                            if !store.is_valid_index(read_index, &read_point) {
                                let rollback_target =
                                    store.last_rollback_target().unwrap_or(Point::Origin);
                                let tip = store.tip();
                                read_index = store.index_of(&rollback_target);
                                read_point = Some(rollback_target.clone());
                                let _ = runner
                                    .send(&CsMsg::MsgRollBackward {
                                        point: rollback_target,
                                        tip,
                                    })
                                    .await;
                                break;
                            }
                            let pending = store.blocks_after(read_index);
                            if let Some(block) = pending.first() {
                                read_index = store.index_of(&block.point);
                                read_point = Some(block.point.clone());
                                let tip = store.tip();
                                send_roll_forward(
                                    &mut runner,
                                    block.header.clone(),
                                    &block.point,
                                    tip,
                                    peer,
                                    behaviour.as_ref(),
                                )
                                .await;
                                break;
                            }
                        }
                    }
                }
            }
            CsMsg::MsgDone => break,
            _ => break,
        }
    }
}

/// Apply the behaviour's per-peer outbound transform to one
/// `MsgRollForward` and dispatch the resulting send(s).  Wraps the four
/// [`OutboundDecision`] variants:
///
/// - `Send`: send the original header + tip.
/// - `Drop`: emit nothing for this peer.
/// - `Replace`: rebuild a `WrappedHeader` from the substituted bytes
///   and re-derive the tip from its point; block number is borrowed
///   from the original tip (variants live at the same chain position).
/// - `Augment`: send the original then each extra in order.
async fn send_roll_forward(
    runner: &mut Runner<ChainSync>,
    header: WrappedHeader,
    block_point: &Point,
    tip: Tip,
    peer: PeerId,
    behaviour: Option<&BehaviourHandle>,
) {
    let Some(handle) = behaviour else {
        let _ = runner.send(&CsMsg::MsgRollForward { header, tip }).await;
        return;
    };

    let slot = match block_point {
        Point::Specific { slot, .. } => *slot,
        Point::Origin => 0,
    };
    let decision = {
        let mut guard = handle.lock().expect("behaviour mutex poisoned");
        guard.transform_outbound(
            peer,
            Outbound::RbHeader {
                slot,
                header: &header.raw,
            },
        )
    };

    match decision {
        OutboundDecision::Send => {
            let _ = runner.send(&CsMsg::MsgRollForward { header, tip }).await;
        }
        OutboundDecision::Drop => {
            tracing::debug!(
                peer = peer.0,
                slot,
                "behaviour dropped chainsync roll-forward"
            );
        }
        OutboundDecision::Replace(OwnedOutbound::RbHeader {
            slot: _,
            header: new_bytes,
        }) => {
            let new_header = WrappedHeader::new(new_bytes);
            let new_tip = match new_header.point() {
                Some(p) => Tip {
                    point: p,
                    block_no: tip.block_no,
                },
                None => tip,
            };
            tracing::info!(
                peer = peer.0,
                slot,
                "behaviour replaced chainsync roll-forward header for peer"
            );
            let _ = runner
                .send(&CsMsg::MsgRollForward {
                    header: new_header,
                    tip: new_tip,
                })
                .await;
        }
        OutboundDecision::Augment(extras) => {
            let _ = runner
                .send(&CsMsg::MsgRollForward {
                    header: header.clone(),
                    tip: tip.clone(),
                })
                .await;
            for extra in extras {
                let OwnedOutbound::RbHeader {
                    slot: _,
                    header: extra_bytes,
                } = extra;
                let extra_header = WrappedHeader::new(extra_bytes);
                let extra_tip = match extra_header.point() {
                    Some(p) => Tip {
                        point: p,
                        block_no: tip.block_no,
                    },
                    None => tip.clone(),
                };
                let _ = runner
                    .send(&CsMsg::MsgRollForward {
                        header: extra_header,
                        tip: extra_tip,
                    })
                    .await;
            }
        }
    }
}

/// Serve BlockFetch for one connection.
///
/// Responds to range requests by streaming blocks from the chain store.
/// When `behaviour` is `Some` and the store has nothing for a
/// single-block range, the behaviour is consulted via
/// [`Behaviour::find_variant_body`](shared_consensus::behaviour::Behaviour::find_variant_body)
/// — lets a peer-split equivocator serve the body of an advertised
/// variant that the local chain selection did not adopt.
pub async fn serve_blockfetch(
    bf_send: CodecSend,
    bf_recv: CodecRecv,
    store: Arc<ChainStore>,
    peer: PeerId,
    behaviour: Option<BehaviourHandle>,
) {
    let mut runner = Runner::<BlockFetch>::new(Role::Server, bf_send, bf_recv);
    let mut first_recv = true;

    loop {
        let msg = match runner.recv().await {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    peer = peer.0,
                    err = %e,
                    "blockfetch: serve_blockfetch recv error, exiting"
                );
                break;
            }
        };
        if first_recv {
            tracing::info!(peer = peer.0, "blockfetch: downstream opened protocol with us");
            first_recv = false;
        }

        match msg {
            BfMsg::MsgRequestRange { from, to } => {
                tracing::info!(
                    peer = peer.0,
                    %from,
                    %to,
                    "blockfetch: downstream requested range (wants blocks from us)"
                );
                let blocks = store.get_range(&from, &to);
                if !blocks.is_empty() {
                    let _ = runner.send(&BfMsg::MsgStartBatch).await;
                    for block in &blocks {
                        let _ = runner
                            .send(&BfMsg::MsgBlock {
                                body: block.body.clone(),
                            })
                            .await;
                    }
                    let _ = runner.send(&BfMsg::MsgBatchDone).await;
                } else if let Some(body) = lookup_variant_body(behaviour.as_ref(), &from, &to) {
                    // Behaviour-side fallback: the requested header is
                    // a peer-split variant that never entered the local
                    // chain tree.
                    let _ = runner.send(&BfMsg::MsgStartBatch).await;
                    let _ = runner.send(&BfMsg::MsgBlock { body }).await;
                    let _ = runner.send(&BfMsg::MsgBatchDone).await;
                } else {
                    let _ = runner.send(&BfMsg::MsgNoBlocks).await;
                }
            }
            BfMsg::MsgClientDone => break,
            _ => break,
        }
    }
}

/// If the behaviour stashed a variant whose header hashes to `from`
/// (and `from == to`, a single-block request), return its body wrapped
/// in a `BlockBody`.  Used as a fallback when the local chain store
/// has nothing for the requested range.
fn lookup_variant_body(
    behaviour: Option<&BehaviourHandle>,
    from: &Point,
    to: &Point,
) -> Option<BlockBody> {
    if from != to {
        return None;
    }
    let (slot, hash) = match from {
        Point::Specific { slot, hash } => (*slot, *hash),
        Point::Origin => return None,
    };
    let handle = behaviour?;
    let guard = handle.lock().expect("behaviour mutex poisoned");
    let body_bytes = guard.find_variant_body(slot, &hash)?;
    drop(guard);
    Some(BlockBody::new(body_bytes))
}

/// Serve KeepAlive for one connection.  Stateless echo.
///
/// The first `MsgKeepAlive` is awaited with **no timeout**: cardano-node
/// doesn't activate the keepalive miniprotocol on cold/warm peers, so
/// applying the 97s `TIMEOUT_CLIENT` deadline from connection time
/// would kill the responder before the first hot promotion ever lands
/// (~15 min in).  Once we've echoed the first round, subsequent rounds
/// use the normal state-driven timeout (`recv()` here) so the liveness
/// watchdog still fires if the relay goes silent after starting the
/// protocol.
///
/// On exit, the task drops its ingress channel.  The outer duplex_task
/// watches `ka_server`'s JoinHandle and tears down the whole connection
/// when this function returns — that's the watchdog's actual effect.
pub async fn serve_keepalive(ka_send: CodecSend, ka_recv: CodecRecv, peer: PeerId) {
    let mut runner = Runner::<KeepAlive>::new(Role::Server, ka_send, ka_recv);
    tracing::info!(peer = peer.0, "cardano-keepalive: serve_keepalive task started");

    let mut msg_count: u64 = 0;
    let exit_reason: &'static str = loop {
        let recv_result = if msg_count == 0 {
            runner.recv_untimed().await
        } else {
            runner.recv().await
        };
        match recv_result {
            Ok(KaMsg::MsgKeepAlive { cookie }) => {
                msg_count += 1;
                // First MsgKeepAlive marks the downstream becoming hot
                // (~15 min after connect) — surface at info so the hot
                // window is visible in operator logs.  Subsequent
                // rounds drop to debug to avoid one log per ~10s per
                // active peer.
                if msg_count == 1 {
                    tracing::info!(
                        peer = peer.0,
                        cookie,
                        "cardano-keepalive: first MsgKeepAlive received (downstream is now hot)"
                    );
                } else {
                    tracing::debug!(
                        peer = peer.0,
                        msg_count,
                        cookie,
                        "cardano-keepalive: received MsgKeepAlive, echoing"
                    );
                }
                match runner.send(&KaMsg::MsgKeepAliveResponse { cookie }).await {
                    Ok(()) => {
                        tracing::debug!(
                            peer = peer.0,
                            cookie,
                            "cardano-keepalive: sent MsgKeepAliveResponse"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            peer = peer.0,
                            cookie,
                            err = %e,
                            "cardano-keepalive: failed to send MsgKeepAliveResponse"
                        );
                        break "send-error";
                    }
                }
            }
            Ok(KaMsg::MsgDone) => break "client-done",
            Ok(other) => {
                tracing::warn!(
                    peer = peer.0,
                    ?other,
                    "cardano-keepalive: unexpected message variant, terminating"
                );
                break "unexpected-message";
            }
            Err(e) => {
                tracing::warn!(
                    peer = peer.0,
                    msg_count,
                    err = %e,
                    "cardano-keepalive: recv error, exiting"
                );
                break "recv-error";
            }
        }
    };
    tracing::info!(
        peer = peer.0,
        msg_count,
        reason = exit_reason,
        "cardano-keepalive: serve_keepalive task exiting"
    );
}

/// Serve TxSubmission for one connection (transaction consumer).
///
/// Pulls transactions from the client and forwards them as
/// `PeerEvent::TransactionReceived` to the coordinator.
pub async fn serve_txsubmission(
    ts_send: CodecSend,
    ts_recv: CodecRecv,
    peer_id: PeerId,
    event_sender: mpsc::Sender<(PeerId, PeerEvent)>,
) {
    let mut runner = Runner::<TxSubmission>::new(Role::Server, ts_send, ts_recv);

    // Receive MsgInit.
    let msg = match runner.recv().await {
        Ok(msg) => msg,
        Err(e) => {
            tracing::warn!(
                peer = peer_id.0,
                err = %e,
                "txsubmission: serve_txsubmission recv MsgInit failed, exiting"
            );
            return;
        }
    };
    if !matches!(msg, TsMsg::MsgInit) {
        tracing::warn!(
            peer = peer_id.0,
            ?msg,
            "txsubmission: expected MsgInit, got other variant, exiting"
        );
        return;
    }
    tracing::info!(
        peer = peer_id.0,
        "txsubmission: downstream opened protocol with us (MsgInit)"
    );

    let mut outstanding: usize = 0;

    loop {
        let (ack, req) = if outstanding > 0 {
            let ack = outstanding as u16;
            outstanding = 0;
            (ack, txsubmission::MAX_UNACKED as u16)
        } else {
            (0u16, txsubmission::MAX_UNACKED as u16)
        };

        let blocking = outstanding == 0 && ack == 0;
        if blocking {
            runner
                .send(&TsMsg::MsgRequestTxIdsBlocking { ack, req })
                .await
                .ok();
        } else {
            runner
                .send(&TsMsg::MsgRequestTxIdsNonBlocking { ack, req })
                .await
                .ok();
        }

        let msg = match runner.recv().await {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    peer = peer_id.0,
                    err = %e,
                    "txsubmission: recv after MsgRequestTxIds* failed"
                );
                break;
            }
        };

        match msg {
            TsMsg::MsgReplyTxIds { tx_ids } => {
                if tx_ids.is_empty() {
                    // Non-blocking empty reply — do a blocking request next.
                    runner
                        .send(&TsMsg::MsgRequestTxIdsBlocking {
                            ack: 0,
                            req: txsubmission::MAX_UNACKED as u16,
                        })
                        .await
                        .ok();

                    let msg = match runner.recv().await {
                        Ok(msg) => msg,
                        Err(e) => {
                            tracing::warn!(
                                peer = peer_id.0,
                                err = %e,
                                "txsubmission: recv after non-blocking-empty retry failed"
                            );
                            break;
                        }
                    };

                    match msg {
                        TsMsg::MsgDone => break,
                        TsMsg::MsgReplyTxIds { tx_ids } if !tx_ids.is_empty() => {
                            let ids: Vec<_> = tx_ids.iter().map(|t| t.tx_id.clone()).collect();
                            let count = ids.len();
                            runner
                                .send(&TsMsg::MsgRequestTxs { tx_ids: ids })
                                .await
                                .ok();

                            let msg = match runner.recv().await {
                                Ok(msg) => msg,
                                Err(e) => {
                                    tracing::warn!(
                                        peer = peer_id.0,
                                        err = %e,
                                        "txsubmission: recv MsgReplyTxs failed (retry path)"
                                    );
                                    break;
                                }
                            };
                            match msg {
                                TsMsg::MsgReplyTxs { txs } => {
                                    for tx in &txs {
                                        let _ = event_sender
                                            .send((
                                                peer_id,
                                                PeerEvent::TransactionReceived {
                                                    body: tx.0.clone(),
                                                },
                                            ))
                                            .await;
                                    }
                                    outstanding = count;
                                }
                                _ => break,
                            }
                        }
                        _ => break,
                    }
                    continue;
                }

                let ids: Vec<_> = tx_ids.iter().map(|t| t.tx_id.clone()).collect();
                let count = ids.len();
                runner
                    .send(&TsMsg::MsgRequestTxs { tx_ids: ids })
                    .await
                    .ok();

                let msg = match runner.recv().await {
                    Ok(msg) => msg,
                    Err(e) => {
                        tracing::warn!(
                            peer = peer_id.0,
                            err = %e,
                            "txsubmission: recv MsgReplyTxs failed"
                        );
                        break;
                    }
                };
                match msg {
                    TsMsg::MsgReplyTxs { txs } => {
                        for tx in &txs {
                            let _ = event_sender
                                .send((
                                    peer_id,
                                    PeerEvent::TransactionReceived { body: tx.0.clone() },
                                ))
                                .await;
                        }
                        outstanding = count;
                    }
                    other => {
                        tracing::warn!(
                            peer = peer_id.0,
                            ?other,
                            "txsubmission: unexpected message awaiting MsgReplyTxs, exiting"
                        );
                        break;
                    }
                }
            }
            TsMsg::MsgDone => break,
            _ => break,
        }
    }
}

/// Serve PeerSharing for one connection.
///
/// Uses a callback to provide peer addresses on request.
pub async fn serve_peersharing(
    ps_send: CodecSend,
    ps_recv: CodecRecv,
    peer: PeerId,
    peer_provider: Arc<dyn Fn(u8) -> Vec<PeerAddress> + Send + Sync>,
) {
    let mut runner = Runner::<PeerSharing>::new(Role::Server, ps_send, ps_recv);
    let mut first_recv = true;

    loop {
        let msg = match runner.recv().await {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    peer = peer.0,
                    err = %e,
                    "peersharing: serve_peersharing recv error, exiting"
                );
                break;
            }
        };
        if first_recv {
            tracing::info!(peer = peer.0, "peersharing: downstream opened protocol with us");
            first_recv = false;
        }

        match msg {
            PsMsg::MsgShareRequest { amount } => {
                tracing::info!(
                    peer = peer.0,
                    amount,
                    "peersharing: downstream requested peers from us"
                );
                let peers = peer_provider(amount);
                let _ = runner.send(&PsMsg::MsgSharePeers { peers }).await;
            }
            PsMsg::MsgDone => break,
            other => {
                tracing::warn!(
                    peer = peer.0,
                    ?other,
                    "peersharing: unexpected message variant, terminating"
                );
                break;
            }
        }
    }
}

/// Translate a stored notification into the LeiosNotify wire message.
/// `eb_size` is the byte length captured at `inject_block` time —
/// CIP-0164 requires the real size; advertising `0` makes the dev relay
/// drop the connection.
fn notification_to_ln_msg(n: &crate::store::leios_store::LeiosNotification) -> LnMsg {
    use crate::store::leios_store::LeiosNotification;
    match n {
        LeiosNotification::BlockOffer { point, eb_size } => LnMsg::MsgLeiosBlockOffer {
            point: point.clone(),
            eb_size: *eb_size,
        },
        LeiosNotification::BlockTxsOffer { point } => LnMsg::MsgLeiosBlockTxsOffer {
            point: point.clone(),
        },
        LeiosNotification::Votes { votes } => LnMsg::MsgLeiosVotes {
            votes: votes.clone(),
        },
    }
}

/// Serve LeiosNotify for one connection.
///
/// Sends notifications about available Leios data as the store is populated.
/// Uses `LeiosStore::subscribe()` to wake when new items are injected.
///
/// `peer` is the connected peer — notifications tagged with that peer
/// as their source are skipped (no-echo gossip), so a duplex follower
/// doesn't immediately re-offer fetched data back to its source.
pub async fn serve_leios_notify(
    ln_send: CodecSend,
    ln_recv: CodecRecv,
    store: Arc<LeiosStore>,
    peer: PeerId,
) {
    let mut runner = Runner::<LeiosNotify>::new(Role::Server, ln_send, ln_recv);
    // `None` until the first MsgLeiosNotificationRequestNext arrives, at
    // which point we snap forward to `store.notification_count()` —
    // skipping every notification that landed in the queue *before* the
    // upstream peer-selection governor promoted this connection to hot
    // and started polling us.
    //
    // Why: a peer can sit cold/warm for the full ~15-min churn cycle
    // while OTHER connections to the same node are hot and pulling EBs
    // into the shared LeiosStore.  Those EBs push notification entries
    // that our serve_leios_notify task can't deliver — nobody is asking.
    // Without this snap, the first LNRequestNext after eventual hot
    // promotion would drain the entire backlog in a single burst, often
    // 10+ MsgLeiosBlockOffer messages in <100ms.  Observed behaviour:
    // the dev relay either rate-limits this burst into a tight hot
    // window or treats us as misbehaving and disconnects within ~1s of
    // promotion (the "quick-drop" cluster we measured on 2026-06-17).
    //
    // Snapping forward also matches the wire semantics a downstream
    // peer would expect: LeiosNotify is for offers fresh enough to be
    // useful; anything older than ~15 min is already past the EB
    // freshness window and the peer learns about it via the RB cert on
    // chainsync instead.
    let mut read_index: Option<usize> = None;
    let mut subscription = store.subscribe();
    let mut first_recv = true;

    loop {
        let msg = match runner.recv().await {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    peer = peer.0,
                    err = %e,
                    "leios_notify: serve_leios_notify recv error, exiting"
                );
                break;
            }
        };
        if first_recv {
            tracing::info!(peer = peer.0, "leios_notify: downstream opened protocol with us");
            first_recv = false;
        }

        match msg {
            LnMsg::MsgLeiosNotificationRequestNext => {
                let cursor = read_index.get_or_insert_with(|| {
                    let snapped = store.notification_count();
                    if snapped > 0 {
                        tracing::info!(
                            peer = peer.0,
                            snapped_past = snapped,
                            "leios_notify: first request — snapping cursor to queue tail, \
                             skipping backlog accumulated while cold/warm"
                        );
                    }
                    snapped
                });
                if let Some(response) =
                    next_outbound_notification(&store, cursor, peer, &mut subscription).await
                {
                    if runner.send(&response).await.is_err() {
                        break;
                    }
                } else {
                    return;
                }
            }
            LnMsg::MsgDone => break,
            _ => break,
        }
    }
}

/// Lazily formats a peer-source list as bare ids (`[1, 2]`) without
/// allocating — only materialised when the tracing level is enabled.
struct SourceIds<'a>(&'a [PeerId]);
impl std::fmt::Debug for SourceIds<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.iter().map(|p| p.0)).finish()
    }
}

/// Find the next notification the wire actually sends to `peer`, skipping
/// entries where `peer` is in the entry's `sources` (no-echo). Advances
/// `read_index` past every skipped entry so the same notification isn't
/// reconsidered next call. Awaits new injects when the queue is empty.
/// Returns `None` on watch-channel close — the server task should exit.
async fn next_outbound_notification(
    store: &Arc<LeiosStore>,
    read_index: &mut usize,
    peer: PeerId,
    subscription: &mut tokio::sync::watch::Receiver<u64>,
) -> Option<LnMsg> {
    use crate::store::leios_store::LeiosNotification;
    loop {
        let pending = store.notifications_after(read_index);
        for entry in &pending {
            *read_index += 1;
            if entry.sources.contains(&peer) {
                tracing::debug!(
                    peer = peer.0,
                    "leios_notify: skipping no-echo offer back to source"
                );
                continue;
            }
            let sources = SourceIds(&entry.sources);
            match &entry.notification {
                LeiosNotification::BlockOffer { point, eb_size } => {
                    tracing::info!(
                        peer = peer.0,
                        %point,
                        eb_size,
                        ?sources,
                        "leios_notify: serving EB offer"
                    );
                }
                LeiosNotification::BlockTxsOffer { point } => {
                    tracing::info!(
                        peer = peer.0,
                        %point,
                        ?sources,
                        "leios_notify: serving EB-txs offer"
                    );
                }
                LeiosNotification::Votes { votes } => {
                    tracing::debug!(
                        peer = peer.0,
                        count = votes.len(),
                        ?sources,
                        "leios_notify: serving votes"
                    );
                }
            }
            return Some(notification_to_ln_msg(&entry.notification));
        }
        // Drained without finding a deliverable entry — wait for new
        // injects before trying again.
        if subscription.changed().await.is_err() {
            return None;
        }
    }
}

/// Serve LeiosFetch for one connection.
///
/// Responds to block and vote fetch requests from the Leios store.
pub async fn serve_leios_fetch(
    lf_send: CodecSend,
    lf_recv: CodecRecv,
    store: Arc<LeiosStore>,
    peer: PeerId,
) {
    let mut runner = Runner::<LeiosFetch>::new(Role::Server, lf_send, lf_recv);
    let mut first_recv = true;

    loop {
        let msg = match runner.recv().await {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!(
                    peer = peer.0,
                    err = %e,
                    "leios_fetch: serve_leios_fetch recv error, exiting"
                );
                break;
            }
        };
        if first_recv {
            tracing::info!(peer = peer.0, "leios_fetch: downstream opened protocol with us");
            first_recv = false;
        }

        match msg {
            LfMsg::MsgLeiosBlockRequest { point } => {
                tracing::info!(peer = peer.0, %point, "leios_fetch: downstream requested EB body from us");
                let block = match &point {
                    Point::Specific { slot, hash } => store.get_block(*slot, hash),
                    Point::Origin => None,
                };
                let Some(block) = block else {
                    // CIP-0164: server should disconnect if it doesn't have the requested EB.
                    break;
                };
                if runner.send(&LfMsg::MsgLeiosBlock { block }).await.is_err() {
                    break;
                }
            }
            LfMsg::MsgLeiosBlockTxsRequest { point, bitmap } => {
                tracing::info!(
                    peer = peer.0,
                    %point,
                    bitmap_chunks = bitmap.len(),
                    "leios_fetch: downstream requested EB txs from us"
                );
                let transactions = match &point {
                    Point::Specific { slot, hash } => store.get_block_txs(*slot, hash, &bitmap),
                    Point::Origin => None,
                };
                let Some(transactions) = transactions else {
                    // CIP-0164: server should disconnect if it doesn't have the requested EB txs.
                    break;
                };
                // Echo the request's point + bitmap alongside the txs.
                if runner
                    .send(&LfMsg::MsgLeiosBlockTxs {
                        point,
                        bitmap,
                        transactions,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            LfMsg::MsgDone => break,
            _ => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bearer::mem::MemBearer;
    use crate::mux::scheduler::{RoundRobin, TrafficClass};
    use crate::mux::{Mux, MuxConfig, ProtocolConfig, MODE_INITIATOR, MODE_RESPONDER};
    use crate::protocols::blockfetch;
    use crate::protocols::chainsync;
    use crate::protocols::keepalive;
    use crate::protocols::leios_fetch::{self, LeiosFetch};
    use crate::protocols::leios_notify::{self, LeiosNotify};
    use crate::types::{BlockBody, Point, WrappedHeader};

    fn make_point(slot: u64) -> Point {
        Point::Specific {
            slot,
            hash: [slot as u8; 32],
        }
    }

    /// Create a valid CBOR-encoded WrappedHeader for testing.
    /// Uses CBOR: array(1, unsigned(slot)) = [slot].
    fn make_header(slot: u64) -> WrappedHeader {
        let mut buf = Vec::new();
        minicbor::encode([slot], &mut buf).unwrap();
        WrappedHeader::opaque(buf)
    }

    /// Create a valid CBOR-encoded BlockBody for testing.
    /// Uses CBOR: array(1, unsigned(slot)) = [slot], padded to `size` isn't
    /// needed — we just need valid CBOR that's distinguishable per slot.
    fn make_body(slot: u64, _size: usize) -> BlockBody {
        let mut buf = Vec::new();
        minicbor::encode([slot], &mut buf).unwrap();
        BlockBody::opaque(buf)
    }

    /// Helper: set up a mux pair for a single protocol.
    fn mux_pair_for_protocol(
        proto: &ProtocolConfig,
    ) -> (
        (CodecSend, CodecRecv),
        (CodecSend, CodecRecv),
        crate::mux::RunningMux,
        crate::mux::RunningMux,
    ) {
        let (bearer_a, bearer_b) = MemBearer::pair();
        let mut mux_a = Mux::new(MuxConfig::default(), RoundRobin::default(), MODE_INITIATOR);
        let (send_a, recv_a) = mux_a.register(proto);
        let running_a = mux_a.run(bearer_a);

        let mut mux_b = Mux::new(MuxConfig::default(), RoundRobin::default(), MODE_RESPONDER);
        let (send_b, recv_b) = mux_b.register(proto);
        let running_b = mux_b.run(bearer_b);

        (
            (CodecSend::new(send_a), CodecRecv::new(recv_a)),
            (CodecSend::new(send_b), CodecRecv::new(recv_b)),
            running_a,
            running_b,
        )
    }

    #[tokio::test]
    async fn chainsync_server_responds_to_intersection_and_roll_forward() {
        let cs_proto = ProtocolConfig {
            id: chainsync::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: chainsync::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&cs_proto);

        // Populate chain store.
        let (store, _rx) = ChainStore::new(100);
        let header_1 = make_header(1);
        for slot in 1..=3 {
            store.append_block(
                make_point(slot),
                make_header(slot),
                make_body(slot, 50),
                slot,
            );
        }

        // Start server.
        let server_handle = tokio::spawn(serve_chainsync(
            server_send,
            server_recv,
            store,
            PeerId(0),
            None,
        ));

        // Client: find intersection at Origin.
        let mut client = Runner::<ChainSync>::new(Role::Client, client_send, client_recv);
        let result = chainsync::find_intersection(&mut client, vec![Point::Origin]).await;
        assert!(result.is_ok());
        let (point, tip) = result.unwrap().unwrap();
        assert_eq!(point, Point::Origin);
        assert_eq!(tip.block_no, 3);

        // Client: request next → should get block 1.
        let event = chainsync::request_next(&mut client).await.unwrap();
        match event {
            chainsync::ChainSyncEvent::RollForward { header, tip } => {
                assert_eq!(header.raw, header_1.raw);
                assert_eq!(tip.block_no, 3);
            }
            other => panic!("expected RollForward, got {other:?}"),
        }

        // Clean up.
        let _ = chainsync::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn blockfetch_server_streams_blocks() {
        let bf_proto = ProtocolConfig {
            id: blockfetch::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: blockfetch::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&bf_proto);

        let (store, _rx) = ChainStore::new(100);
        let body_1 = make_body(1, 100);
        let body_3 = make_body(3, 100);
        for slot in 1..=3 {
            store.append_block(
                make_point(slot),
                make_header(slot),
                make_body(slot, 100),
                slot,
            );
        }

        let server_handle =
            tokio::spawn(serve_blockfetch(server_send, server_recv, store, PeerId(0), None));

        let mut client = Runner::<BlockFetch>::new(Role::Client, client_send, client_recv);

        // Request range [1..3].
        let has_blocks = blockfetch::request_range(&mut client, make_point(1), make_point(3)).await;
        assert!(has_blocks.unwrap());

        // Receive 3 blocks.
        let mut received = Vec::new();
        while let Ok(Some(body)) = blockfetch::recv_block(&mut client).await {
            received.push(body);
        }
        assert_eq!(received.len(), 3);
        assert_eq!(received[0].raw, body_1.raw);
        assert_eq!(received[2].raw, body_3.raw);

        let _ = blockfetch::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn keepalive_server_echoes_cookie() {
        let ka_proto = ProtocolConfig {
            id: keepalive::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: keepalive::INGRESS_LIMIT,
            egress_queue_size: 4,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&ka_proto);

        let server_handle = tokio::spawn(serve_keepalive(server_send, server_recv, PeerId(0)));

        let mut client = Runner::<KeepAlive>::new(Role::Client, client_send, client_recv);
        let rtt = keepalive::keep_alive(&mut client, 42).await.unwrap();
        assert!(rtt.as_millis() < 1000); // MemBearer should be fast

        let _ = keepalive::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_notify_server_sends_notifications() {
        let ln_proto = ProtocolConfig {
            id: leios_notify::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_notify::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&ln_proto);

        let (store, _rx) = LeiosStore::new(100);

        // Start server with an empty store — it will snap its cursor
        // forward on the first MsgLeiosNotificationRequestNext.
        let server_handle = tokio::spawn(serve_leios_notify(
            server_send,
            server_recv,
            store.clone(),
            PeerId(0),
        ));

        // Issue request_next concurrently with a delayed inject: the
        // request reaches the server first, the server snaps and starts
        // awaiting fresh notifications, and the inject lands afterwards
        // so the response comes from a post-snap entry.
        let mut client = Runner::<LeiosNotify>::new(Role::Client, client_send, client_recv);
        let inject_store = store.clone();
        let (event, _) = tokio::join!(
            leios_notify::request_next(&mut client),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                inject_store.inject_block(
                    Point::Specific {
                        slot: 42,
                        hash: [0xAB; 32],
                    },
                    vec![1, 2, 3],
                    None,
                );
            }
        );

        match event.unwrap() {
            leios_notify::LeiosNotifyEvent::BlockOffer { point, eb_size } => {
                assert_eq!(
                    point,
                    Point::Specific {
                        slot: 42,
                        hash: [0xAB; 32]
                    }
                );
                assert_eq!(eb_size, 3);
            }
            other => panic!("expected BlockOffer, got {other:?}"),
        }

        let _ = leios_notify::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_notify_server_skips_offers_from_connected_peer() {
        // No-echo: an EB or vote that was sourced from peer P must not
        // be re-offered to P on the same connection. Two notifications
        // are queued — one from PeerId(7), one from PeerId(9). The
        // server is talking to PeerId(7), so only the second one
        // should reach the wire. Without filtering, the duplex
        // follower would reflect a fetched EB back to its source and
        // (because the size was hardcoded to 0) crash the dev relay.
        let ln_proto = ProtocolConfig {
            id: leios_notify::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_notify::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&ln_proto);

        let (store, _rx) = LeiosStore::new(100);
        let connected_peer = PeerId(7);
        let other_peer = PeerId(9);

        let server_handle = tokio::spawn(serve_leios_notify(
            server_send,
            server_recv,
            store.clone(),
            connected_peer,
        ));

        // Inject after the server has snapped its cursor on the first
        // request — both offers land in the post-snap window so the
        // no-echo gate alone decides which one reaches the wire.
        let inject_store = store.clone();
        let mut client = Runner::<LeiosNotify>::new(Role::Client, client_send, client_recv);
        let (event, _) = tokio::join!(
            leios_notify::request_next(&mut client),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                inject_store.inject_block(
                    Point::Specific {
                        slot: 10,
                        hash: [0xAA; 32],
                    },
                    vec![1, 2, 3],
                    Some(connected_peer),
                );
                inject_store.inject_block(
                    Point::Specific {
                        slot: 11,
                        hash: [0xBB; 32],
                    },
                    vec![4, 5, 6, 7],
                    Some(other_peer),
                );
            }
        );

        match event.unwrap() {
            leios_notify::LeiosNotifyEvent::BlockOffer { point, eb_size } => {
                assert_eq!(
                    point,
                    Point::Specific {
                        slot: 11,
                        hash: [0xBB; 32]
                    },
                    "expected the offer from other_peer, not the echo from connected_peer"
                );
                assert_eq!(eb_size, 4, "eb_size should match the injected block length");
            }
            other => panic!("expected BlockOffer, got {other:?}"),
        }

        let _ = leios_notify::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_notify_server_skips_offers_with_connected_peer_in_sources() {
        // Multi-source no-echo: an EB advertised to us by two peers
        // (one of them the connected one) must not be re-offered back,
        // even though the connected peer is just one of multiple
        // sources after dedup.
        let ln_proto = ProtocolConfig {
            id: leios_notify::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_notify::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&ln_proto);

        let (store, _rx) = LeiosStore::new(100);
        let connected_peer = PeerId(7);
        let other_peer = PeerId(9);
        let third_peer = PeerId(13);

        let server_handle = tokio::spawn(serve_leios_notify(
            server_send,
            server_recv,
            store.clone(),
            connected_peer,
        ));

        // Inject after the cursor snap: the no-echo gate then has to
        // decide between two queued entries on its own.
        let inject_store = store.clone();
        let mut client = Runner::<LeiosNotify>::new(Role::Client, client_send, client_recv);
        let (event, _) = tokio::join!(
            leios_notify::request_next(&mut client),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                // Same EB advertised by other_peer then by connected_peer —
                // dedup collapses these into a single entry whose `sources`
                // contains both. The server must still skip it for connected_peer.
                let point = Point::Specific {
                    slot: 10,
                    hash: [0xAA; 32],
                };
                inject_store.inject_block(point.clone(), vec![1, 2, 3], Some(other_peer));
                inject_store.inject_block(point.clone(), vec![1, 2, 3], Some(connected_peer));
                // A separate EB advertised by third_peer — this one must be delivered.
                inject_store.inject_block(
                    Point::Specific {
                        slot: 11,
                        hash: [0xBB; 32],
                    },
                    vec![4, 5, 6, 7],
                    Some(third_peer),
                );
            }
        );

        match event.unwrap() {
            leios_notify::LeiosNotifyEvent::BlockOffer { point, .. } => {
                assert_eq!(
                    point,
                    Point::Specific {
                        slot: 11,
                        hash: [0xBB; 32]
                    },
                    "expected the third-peer offer; the connected_peer-sourced one must be filtered"
                );
            }
            other => panic!("expected BlockOffer, got {other:?}"),
        }

        let _ = leios_notify::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_notify_server_dedups_same_eb_to_one_offer() {
        // Two advertisements from different sources for the same EB
        // must produce a single wire offer downstream. Without dedup
        // the server would emit two MsgLeiosBlockOffer messages.
        let ln_proto = ProtocolConfig {
            id: leios_notify::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_notify::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&ln_proto);

        let (store, _rx) = LeiosStore::new(100);
        let downstream = PeerId(99);
        let point = Point::Specific {
            slot: 21,
            hash: [0x12; 32],
        };

        let server_handle = tokio::spawn(serve_leios_notify(
            server_send,
            server_recv,
            store.clone(),
            downstream,
        ));

        let mut client = Runner::<LeiosNotify>::new(Role::Client, client_send, client_recv);

        // Inject all three (two duped + sentinel) after the cursor
        // snap.  The first request_next then surfaces the deduped
        // offer; the second surfaces the sentinel, proving the duped
        // point did not appear twice.
        let inject_store = store.clone();
        let point_for_inject = point.clone();
        let (first, _) = tokio::join!(
            leios_notify::request_next(&mut client),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                inject_store.inject_block(point_for_inject.clone(), vec![0xCD; 17], Some(PeerId(1)));
                inject_store.inject_block(point_for_inject, vec![0xCD; 17], Some(PeerId(2)));
                inject_store.inject_block(
                    Point::Specific {
                        slot: 22,
                        hash: [0x34; 32],
                    },
                    vec![0xEF; 5],
                    Some(PeerId(3)),
                );
            }
        );

        match first.unwrap() {
            leios_notify::LeiosNotifyEvent::BlockOffer { point: p, eb_size } => {
                assert_eq!(p, point);
                assert_eq!(eb_size, 17);
            }
            other => panic!("expected first BlockOffer for duped point, got {other:?}"),
        }

        let second = leios_notify::request_next(&mut client).await.unwrap();
        match second {
            leios_notify::LeiosNotifyEvent::BlockOffer { point: p, .. } => {
                assert_eq!(
                    p,
                    Point::Specific {
                        slot: 22,
                        hash: [0x34; 32]
                    },
                    "second offer should be the sentinel — the duped point must not appear again"
                );
            }
            other => panic!("expected sentinel BlockOffer, got {other:?}"),
        }

        let _ = leios_notify::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_notify_server_snaps_past_backlog_on_first_request() {
        // Cold-period accumulation must not flood the wire on the
        // first MsgLeiosNotificationRequestNext: every offer that was
        // queued *before* the first request is silently skipped, and
        // only the offer that lands *after* the snap is delivered.
        let ln_proto = ProtocolConfig {
            id: leios_notify::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_notify::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&ln_proto);

        let (store, _rx) = LeiosStore::new(100);
        let downstream = PeerId(42);

        // Pre-server-start backlog: three EBs from a different peer
        // that landed while we were cold/warm.  After the snap, none
        // of these should reach the wire.
        for slot in 1u64..=3 {
            let mut hash = [0u8; 32];
            hash[0] = slot as u8;
            store.inject_block(
                Point::Specific { slot, hash },
                vec![0xAA; 10],
                Some(PeerId(7)),
            );
        }

        let server_handle = tokio::spawn(serve_leios_notify(
            server_send,
            server_recv,
            store.clone(),
            downstream,
        ));

        // Issue request_next + delayed post-snap inject of a sentinel.
        // The sentinel slot (100) is unmistakable; if the snap is
        // broken, the test will surface the slot-1 backlog entry
        // instead and the assertion below catches it.
        let inject_store = store.clone();
        let mut client = Runner::<LeiosNotify>::new(Role::Client, client_send, client_recv);
        let (event, _) = tokio::join!(
            leios_notify::request_next(&mut client),
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                inject_store.inject_block(
                    Point::Specific {
                        slot: 100,
                        hash: [0xEE; 32],
                    },
                    vec![0xBB; 20],
                    Some(PeerId(8)),
                );
            }
        );

        match event.unwrap() {
            leios_notify::LeiosNotifyEvent::BlockOffer { point, eb_size } => {
                assert_eq!(
                    point,
                    Point::Specific {
                        slot: 100,
                        hash: [0xEE; 32]
                    },
                    "expected the post-snap sentinel, got a backlog entry — snap-forward is not engaging"
                );
                assert_eq!(eb_size, 20);
            }
            other => panic!("expected BlockOffer, got {other:?}"),
        }

        let _ = leios_notify::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_fetch_server_delivers_block() {
        let lf_proto = ProtocolConfig {
            id: leios_fetch::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_fetch::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&lf_proto);

        // Create and populate LeiosStore.  The EB blob is a CBOR
        // endorser_block map { hash => size } (empty here).
        let (store, _rx) = LeiosStore::new(100);
        store.inject_block(
            Point::Specific {
                slot: 42,
                hash: [0xAB; 32],
            },
            vec![0xA0],
            None,
        );

        // Start server.
        let server_handle = tokio::spawn(serve_leios_fetch(server_send, server_recv, store, PeerId(0)));

        // Client: fetch block.
        let mut client = Runner::<LeiosFetch>::new(Role::Client, client_send, client_recv);
        let block = leios_fetch::fetch_block(
            &mut client,
            Point::Specific {
                slot: 42,
                hash: [0xAB; 32],
            },
        )
        .await
        .unwrap();
        assert_eq!(block, vec![0xA0]);

        // Clean up.
        let _ = leios_fetch::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_fetch_disconnects_on_missing_block() {
        let lf_proto = ProtocolConfig {
            id: leios_fetch::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_fetch::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&lf_proto);

        // Empty store — no blocks injected.
        let (store, _rx) = LeiosStore::new(100);

        let server_handle = tokio::spawn(serve_leios_fetch(server_send, server_recv, store, PeerId(0)));

        // Client: request a block that doesn't exist.
        let mut client = Runner::<LeiosFetch>::new(Role::Client, client_send, client_recv);
        let result = leios_fetch::fetch_block(
            &mut client,
            Point::Specific {
                slot: 99,
                hash: [0xFF; 32],
            },
        )
        .await;

        // Server should have disconnected — client sees an error.
        assert!(
            result.is_err(),
            "expected error from disconnect, got {result:?}"
        );

        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_fetch_returns_bitmap_subset_of_block_txs() {
        let lf_proto = ProtocolConfig {
            id: leios_fetch::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_fetch::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&lf_proto);

        // Inject 100 transactions for one EB.
        let (store, _rx) = LeiosStore::new(100);
        let hash = [0x77u8; 32];
        let point = Point::Specific { slot: 50, hash };
        // Each tx body is a single valid CBOR value (a 3-byte bytestring,
        // 0x43 = bytes(3)) — the codec passes txs through as raw CBOR.
        let txs: Vec<Vec<u8>> = (0..100u8).map(|i| vec![0x43, i, i, i]).collect();
        store.inject_block_txs_full(point.clone(), txs, None);

        let server_handle =
            tokio::spawn(serve_leios_fetch(server_send, server_recv, store.clone(), PeerId(0)));

        // Client: ask for indices 0, 5, 64, 99.
        let mut client = Runner::<LeiosFetch>::new(Role::Client, client_send, client_recv);
        let bitmap = crate::protocols::leios_fetch::bitmap::from_indices(&[0, 5, 64, 99]);
        let got = leios_fetch::fetch_block_txs(&mut client, point, bitmap)
            .await
            .expect("server should respond");

        // Server returns those four in ascending order.
        assert_eq!(got.len(), 4);
        assert_eq!(got[0], vec![0x43, 0, 0, 0]);
        assert_eq!(got[1], vec![0x43, 5, 5, 5]);
        assert_eq!(got[2], vec![0x43, 64, 64, 64]);
        assert_eq!(got[3], vec![0x43, 99, 99, 99]);

        let _ = leios_fetch::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_fetch_serves_bitmap_via_manifest_and_resolver() {
        use crate::store::leios_store::TxBodyResolver;
        use std::sync::Arc;

        struct StubResolver(std::collections::HashMap<Vec<u8>, Vec<u8>>);
        impl TxBodyResolver for StubResolver {
            fn resolve_body(&self, tx_id: &[u8]) -> Option<Vec<u8>> {
                self.0.get(tx_id).cloned()
            }
        }

        let lf_proto = ProtocolConfig {
            id: leios_fetch::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_fetch::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&lf_proto);

        let h0 = [0xA0u8; 32];
        let h1 = [0xA1u8; 32];
        let h2 = [0xA2u8; 32];
        // Bodies are single valid CBOR values (1-byte bytestrings,
        // 0x41 = bytes(1)) — txs pass through the codec as raw CBOR.
        let resolver: Arc<dyn TxBodyResolver> = Arc::new(StubResolver(
            [
                (h0.to_vec(), vec![0x41, 0xB0]),
                (h1.to_vec(), vec![0x41, 0xB1]),
                (h2.to_vec(), vec![0x41, 0xB2]),
            ]
            .into_iter()
            .collect(),
        ));
        // Receiver-style store: only the manifest is recorded; bodies
        // come from the resolver.
        let (store, _rx) = LeiosStore::new_with_resolver(100, Some(resolver));
        let hash = [0xEFu8; 32];
        let point = Point::Specific { slot: 33, hash };
        store.record_eb_manifest(point.clone(), vec![h0, h1, h2], None);

        let server_handle =
            tokio::spawn(serve_leios_fetch(server_send, server_recv, store.clone(), PeerId(0)));

        let mut client = Runner::<LeiosFetch>::new(Role::Client, client_send, client_recv);
        let bitmap = crate::protocols::leios_fetch::bitmap::from_indices(&[0, 2]);
        let got = leios_fetch::fetch_block_txs(&mut client, point, bitmap)
            .await
            .expect("server should respond");
        assert_eq!(got, vec![vec![0x41u8, 0xB0], vec![0x41u8, 0xB2]]);

        let _ = leios_fetch::done(&mut client).await;
        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }

    #[tokio::test]
    async fn leios_fetch_disconnects_on_missing_block_txs() {
        let lf_proto = ProtocolConfig {
            id: leios_fetch::PROTOCOL_ID,
            traffic_class: TrafficClass::Priority,
            ingress_limit: leios_fetch::INGRESS_LIMIT,
            egress_queue_size: 16,
        };

        let ((client_send, client_recv), (server_send, server_recv), mux_a, mux_b) =
            mux_pair_for_protocol(&lf_proto);

        // Empty store — no block txs injected.
        let (store, _rx) = LeiosStore::new(100);

        let server_handle = tokio::spawn(serve_leios_fetch(server_send, server_recv, store, PeerId(0)));

        // Client: request txs for a block that doesn't exist.
        let mut client = Runner::<LeiosFetch>::new(Role::Client, client_send, client_recv);
        let result = leios_fetch::fetch_block_txs(
            &mut client,
            Point::Specific {
                slot: 99,
                hash: [0xFF; 32],
            },
            std::collections::BTreeMap::new(),
        )
        .await;

        // Server should have disconnected — client sees an error.
        assert!(
            result.is_err(),
            "expected error from disconnect, got {result:?}"
        );

        server_handle.await.ok();
        mux_a.abort();
        mux_b.abort();
    }
}
