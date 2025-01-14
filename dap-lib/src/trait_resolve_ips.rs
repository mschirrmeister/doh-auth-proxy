use crate::error::*;
use async_trait::async_trait;
use std::net::SocketAddr;
use url::Url;

#[async_trait]
/// Trait that resolves ip addresses from a given url.
/// This will be used both for bootstrap DNS resolver and MODoH resolver itself.
pub trait ResolveIps {
  async fn resolve_ips(&self, target_url: &Url) -> Result<ResolveIpResponse>;
}
/// Response of ResolveIps trait
pub struct ResolveIpResponse {
  /// hostname of target url
  pub hostname: String,
  /// resolved ip addresses
  pub addresses: Vec<SocketAddr>,
}

/// Resolve ip addresses for given endpoints
pub async fn resolve_ips(endpoints: &[Url], resolver_ips: impl ResolveIps) -> Result<Vec<ResolveIpResponse>> {
  let resolve_ips_fut = endpoints.iter().map(|endpoint| resolver_ips.resolve_ips(endpoint));
  let resolve_ips = futures::future::join_all(resolve_ips_fut).await;
  if resolve_ips.iter().any(|resolve_ip| resolve_ip.is_err()) {
    return Err(DapError::FailedToResolveIpsForHttpClient);
  }
  let resolve_ips_vec = resolve_ips.into_iter().map(|resolve_ip| resolve_ip.unwrap()).collect();
  Ok(resolve_ips_vec)
}
