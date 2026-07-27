// SPDX-License-Identifier: AGPL-3.0-only

//! Fail-closed admission of the address `cs-api` listens on.
//!
//! `cs-api` has no authentication: every request that reaches the
//! socket is executed with the operator's own authority, and some of
//! those requests spawn workers. The listening address is therefore not
//! a convenience setting — it *is* the access-control boundary, and the
//! only one the process has.
//!
//! This module makes that boundary a value the binary must construct
//! before it can listen, rather than a string it passes through to the
//! kernel:
//!
//! - [`classify`] sorts an address into [`BindClass`] — loopback,
//!   unspecified (`0.0.0.0` / `::`), or a concrete routable address.
//! - [`admit`] turns a class plus the operator's explicit consent into
//!   either an [`AdmittedBind`] witness or a [`BindRefusal`].
//!
//! The rules, in the order they fire:
//!
//! 1. **Loopback is always admitted.** Nothing outside the machine can
//!    reach it.
//! 2. **Unspecified is always refused**, consent or not. `0.0.0.0` does
//!    not name a network — it names *every* interface the host has now
//!    or acquires later, including ones the operator has not yet
//!    plugged in. There is no address to determine, so we fail closed.
//!    Same refusal, same reason, as
//!    [`apps_transport_http::bind`](../../apps-transport-http/src/bind.rs).
//! 3. **A concrete routable address is refused unless the operator
//!    passed the consent flag.** `--i-know-this-exposes-an-unauthenticated-api`
//!    is deliberately long: it has to be typed on purpose, and it says
//!    what it does at the call site.
//!
//! A [`AdmittedBind`] cannot be constructed except by [`admit`], so the
//! listener in `main` cannot be handed an address that was never
//! examined.

use std::net::SocketAddr;

use thiserror::Error;

/// What kind of address the operator asked us to listen on.
///
/// Exists so the refusal message can name the *reason* rather than
/// restate the address, and so the decision is testable without a
/// socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindClass {
    /// `127.0.0.0/8` or `::1` — reachable only from this machine.
    Loopback,
    /// `0.0.0.0` or `::` — every interface, present and future.
    Unspecified,
    /// A concrete address that some other machine can route to.
    Routable,
}

/// Sort a socket address into its [`BindClass`].
///
/// Pure and total: every `SocketAddr` carries a concrete `IpAddr`, so
/// there is no "undetermined" case to guess at here. The undetermined
/// case is `Unspecified`, and it is a refusal, not an assumption.
#[must_use]
pub fn classify(addr: SocketAddr) -> BindClass {
    let ip = addr.ip();
    if ip.is_loopback() {
        BindClass::Loopback
    } else if ip.is_unspecified() {
        BindClass::Unspecified
    } else {
        BindClass::Routable
    }
}

/// Why `cs-api` declined to listen on the requested address.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BindRefusal {
    /// The operator asked for `0.0.0.0` / `::`.
    #[error(
        "refusing to bind {0}: `0.0.0.0` / `::` is every interface this host has now \
         or acquires later, so the exposure cannot be determined. cs-api has no \
         authentication and POST /molecules/{{id}}/tackle spawns a worker. Bind \
         loopback (127.0.0.1:4222), or name one concrete interface — a Tailscale \
         address, `tailscale ip -4` — and pass \
         --i-know-this-exposes-an-unauthenticated-api."
    )]
    Unspecified(SocketAddr),
    /// A routable address without the consent flag.
    #[error(
        "refusing to bind {0}: that address is reachable from other machines and \
         cs-api has no authentication — anyone who can route to it can spawn \
         workers and spend your credit. Bind loopback (127.0.0.1:4222), or, if \
         this address is inside a private trust boundary you control (a tailnet), \
         pass --i-know-this-exposes-an-unauthenticated-api."
    )]
    UnauthenticatedExposure(SocketAddr),
}

/// A bind address that has passed [`admit`].
///
/// The inner field is private: the only way to obtain one is to have
/// gone through the check. `main` takes this, not a `SocketAddr`, so
/// "we forgot to validate" is not a state the binary can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmittedBind {
    addr: SocketAddr,
    class: BindClass,
}

impl AdmittedBind {
    /// The address to hand to the listener.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The class it was admitted as. `Routable` means the operator
    /// consented to an unauthenticated exposure and callers may want to
    /// say so on stderr.
    #[must_use]
    pub fn class(&self) -> BindClass {
        self.class
    }
}

/// Decide whether `cs-api` may listen on `addr`.
///
/// `consented` is the operator's explicit gesture
/// (`--i-know-this-exposes-an-unauthenticated-api`). It widens exactly
/// one case — a concrete routable address — and never the unspecified
/// one.
///
/// # Errors
///
/// Returns [`BindRefusal::Unspecified`] for `0.0.0.0` / `::` regardless
/// of consent, and [`BindRefusal::UnauthenticatedExposure`] for a
/// routable address without consent.
pub fn admit(addr: SocketAddr, consented: bool) -> Result<AdmittedBind, BindRefusal> {
    let class = classify(addr);
    match class {
        BindClass::Loopback => Ok(AdmittedBind { addr, class }),
        BindClass::Unspecified => Err(BindRefusal::Unspecified(addr)),
        BindClass::Routable if consented => Ok(AdmittedBind { addr, class }),
        BindClass::Routable => Err(BindRefusal::UnauthenticatedExposure(addr)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn v4(ip: [u8; 4]) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 4222)
    }

    #[test]
    fn loopback_is_admitted_without_consent() {
        let admitted = admit(v4([127, 0, 0, 1]), false).expect("loopback is the default");
        assert_eq!(admitted.class(), BindClass::Loopback);
        assert_eq!(admitted.addr(), v4([127, 0, 0, 1]));
    }

    #[test]
    fn ipv6_loopback_is_admitted() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 4222);
        assert_eq!(
            admit(addr, false).expect("::1 is loopback").class(),
            BindClass::Loopback
        );
    }

    /// The load-bearing case: consent does **not** buy `0.0.0.0`. An
    /// operator who wants reach must name the interface they mean.
    #[test]
    fn unspecified_is_refused_even_with_consent() {
        let addr = v4([0, 0, 0, 0]);
        assert_eq!(admit(addr, false), Err(BindRefusal::Unspecified(addr)));
        assert_eq!(admit(addr, true), Err(BindRefusal::Unspecified(addr)));
    }

    #[test]
    fn ipv6_unspecified_is_refused_even_with_consent() {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 4222);
        assert_eq!(admit(addr, true), Err(BindRefusal::Unspecified(addr)));
    }

    #[test]
    fn routable_needs_the_explicit_gesture() {
        let tailscale = v4([100, 64, 0, 12]);
        assert_eq!(
            admit(tailscale, false),
            Err(BindRefusal::UnauthenticatedExposure(tailscale))
        );
        assert_eq!(
            admit(tailscale, true)
                .expect("consent admits a concrete address")
                .class(),
            BindClass::Routable
        );
    }

    /// A LAN address is as routable as a public one — the check is
    /// about reachability by another machine, not about RFC1918.
    #[test]
    fn private_lan_address_is_routable_not_loopback() {
        assert_eq!(classify(v4([192, 168, 1, 7])), BindClass::Routable);
    }

    #[test]
    fn refusal_message_names_the_hazard() {
        let err = admit(v4([0, 0, 0, 0]), true).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("no authentication"), "{text}");
        assert!(text.contains("127.0.0.1:4222"), "{text}");
    }
}
