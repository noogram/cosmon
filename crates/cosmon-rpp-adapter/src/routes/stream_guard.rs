// SPDX-License-Identifier: AGPL-3.0-only

//! Live admission guard shared by both tenant SSE routes.
//!
//! A request is admitted once, including rate limiting and inbox
//! materialisation. An open stream then rechecks the current trust and
//! policy projections without consuming another rate-limit token or
//! writing another inbox record. The timer also wakes an idle stream.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::response::sse::Event;
use tokio::time::Interval;
use tokio_stream::Stream;

use crate::admission::Spark;
use crate::jwt::JwtVerifier;
use crate::rate_limit::hash_sub;
use crate::AppState;

/// Recheck interval for open streams. The deny-list cache has its own
/// 30-second TTL, so an operator file edit closes a stream within
/// 31 seconds; a loaded binding or issuer change and JWT expiry close
/// within one second of becoming effective.
const RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// An SSE source whose output remains subject to current admission.
/// Source completion remains completion (including `follow=false`).
pub(super) struct GuardedStream<S> {
    source: Pin<Box<S>>,
    interval: Interval,
    state: Arc<AppState>,
    token: String,
    spark: Spark,
    scope: &'static str,
    route: &'static str,
}

/// Wrap an admitted SSE source with periodic and per-item rechecks.
pub(super) fn guard_stream<S>(
    source: S,
    state: Arc<AppState>,
    token: String,
    spark: Spark,
    scope: &'static str,
    route: &'static str,
) -> GuardedStream<S>
where
    S: Stream<Item = Result<Event, Infallible>>,
{
    GuardedStream {
        source: Box::pin(source),
        interval: tokio::time::interval(RECHECK_INTERVAL),
        state,
        token,
        spark,
        scope,
        route,
    }
}

impl<S> Stream for GuardedStream<S>
where
    S: Stream<Item = Result<Event, Infallible>>,
{
    type Item = Result<Event, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // Polling the interval registers a wake even when the source is
        // idle. Recheck on every poll so a queued item cannot outrun a
        // revocation tick.
        let _ = Pin::new(&mut this.interval).poll_tick(cx);
        if let Err(reason) = admission_still_valid(this) {
            tracing::warn!(
                event = "rpp.sse.admission_ended",
                route = this.route,
                reason,
                "closing tenant stream after admission changed"
            );
            return Poll::Ready(None);
        }
        this.source.as_mut().poll_next(cx)
    }
}

fn admission_still_valid<S>(stream: &GuardedStream<S>) -> Result<(), &'static str> {
    let jwt = JwtVerifier::validate(
        &stream.state.jwks.load(),
        &stream.token,
        stream.state.posture,
    )
    .map_err(|_| "credential")?;
    let map = stream.state.nucleon_map.load();
    let binding = map
        .resolve_for_audience(&jwt.iss, &jwt.sub, &jwt.aud)
        .ok_or("binding")?;
    if binding.noyau != stream.spark.noyau || binding.nucleon_id.as_str() != stream.spark.nucleon_id
    {
        return Err("binding");
    }
    if !jwt.has_scope(stream.scope) && !binding.allowed_scopes.iter().any(|s| s == stream.scope) {
        return Err("scope");
    }
    let deny = stream.state.deny_list.snapshot();
    let sub_hash = hash_sub(&jwt.sub);
    if deny.global_kill
        || deny.revokes_sub(&jwt.iss, &sub_hash)
        || deny.revokes_jti(&jwt.iss, &sub_hash, &jwt.jti)
        || deny
            .denied_noyaus
            .iter()
            .any(|n| n == stream.spark.noyau.as_str())
    {
        return Err("policy");
    }
    Ok(())
}
