//! Asynchronous DNS resolver

use std::{
    io::{self, Error, ErrorKind},
    net::SocketAddr,
};

use log::{error, trace};
use trust_dns_resolver::{
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
    TokioAsyncResolver,
};

use crate::context::Context;

use super::tokio_dns_resolver::resolve as tokio_resolve;

/// Create a `trust-dns` asynchronous DNS resolver
pub async fn create_resolver(dns: Option<ResolverConfig>, ipv6_first: bool) -> io::Result<TokioAsyncResolver> {
    let mut resolver_opts = ResolverOpts::default();

    if ipv6_first {
        resolver_opts.ip_strategy = LookupIpStrategy::Ipv6thenIpv4;
    }

    // Customized dns resolution
    match dns {
        Some(conf) => {
            trace!(
                "initializing DNS resolver with config {:?} opts {:?}",
                conf,
                resolver_opts
            );
            TokioAsyncResolver::tokio(conf, resolver_opts)
        }

        // To make this independent, if targeting macOS, BSD, Linux, or Windows, we can use the system's configuration
        #[cfg(any(unix, windows))]
        None => {
            use trust_dns_resolver::{name_server::TokioHandle, system_conf::read_system_conf};

            // use the system resolver configuration
            let (config, mut opts) = match read_system_conf() {
                Ok(o) => o,
                Err(err) => {
                    error!("failed to initialize DNS resolver with system-config, error: {}", err);

                    // From::from is required because on error type is different on Windows
                    #[allow(clippy::useless_conversion)]
                    return Err(From::from(err));
                }
            };

            // NOTE: timeout will be set by config (for example, /etc/resolv.conf on UNIX-like system)
            //
            // Only ip_strategy should be changed
            if ipv6_first {
                opts.ip_strategy = LookupIpStrategy::Ipv6thenIpv4;
            }

            trace!(
                "initializing DNS resolver with system-config {:?} opts {:?}",
                config,
                opts
            );

            TokioAsyncResolver::new(config, opts, TokioHandle)
        }

        #[cfg(not(any(unix, windows)))]
        None => {
            // Get a new resolver with the google nameservers as the upstream recursive resolvers
            trace!(
                "initializing DNS resolver with google-config {:?} opts {:?}",
                ResolverConfig::google(),
                resolver_opts
            );
            TokioAsyncResolver::tokio(ResolverConfig::google(), resolver_opts)
        }
    }
    .map_err(From::from)
}

// Resolved result
enum EitherResolved<A, B> {
    Tokio(A),
    Trust(B),
}

impl<A, B> Iterator for EitherResolved<A, B>
where
    A: Iterator<Item = SocketAddr>,
    B: Iterator<Item = SocketAddr>,
{
    type Item = SocketAddr;

    fn next(&mut self) -> Option<SocketAddr> {
        match *self {
            EitherResolved::Tokio(ref mut a) => a.next(),
            EitherResolved::Trust(ref mut b) => b.next(),
        }
    }
}

/// Perform a DNS resolution
pub async fn resolve<'a>(
    context: &Context,
    addr: &'a str,
    port: u16,
) -> io::Result<impl Iterator<Item = SocketAddr> + 'a> {
    match context.dns_resolver() {
        Some(resolver) => {
            trace!("DNS resolving {}:{} with trust-dns", addr, port);

            match resolver.lookup_ip(addr).await {
                Ok(lookup_result) => Ok(EitherResolved::Trust(
                    lookup_result.into_iter().map(move |ip| SocketAddr::new(ip, port)),
                )),
                Err(err) => {
                    let err = Error::new(
                        ErrorKind::Other,
                        format!("dns resolve {}:{} error: {}", addr, port, err),
                    );
                    Err(err)
                }
            }
        }
        // Fallback to tokio's DNS resolver
        None => {
            trace!("DNS resolving {}:{} with tokio (fallback)", addr, port);

            tokio_resolve(context, addr, port).await.map(EitherResolved::Tokio)
        }
    }
}
