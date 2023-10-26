use super::DoHType;
use crate::{error::*, globals::Globals};
use itertools::Itertools;
use std::sync::{
  atomic::{AtomicBool, Ordering},
  Arc,
};
use url::Url;

/// scheme
enum Scheme {
  Http,
  Https,
}
impl Scheme {
  pub fn as_str(&self) -> &'static str {
    match self {
      Scheme::Http => "http",
      Scheme::Https => "https",
    }
  }
}
impl TryFrom<&str> for Scheme {
  type Error = DapError;
  fn try_from(s: &str) -> Result<Self> {
    match s {
      "http" => Ok(Self::Http),
      "https" => Ok(Self::Https),
      _ => Err(DapError::FailedToBuildDohUrl),
    }
  }
}
/// DoH target resolver
struct DoHTarget {
  /// authority like "dns.google:443"
  authority: String,
  /// path like "/dns-query" that must start from "/"
  path: String,
  /// scheme
  scheme: Scheme,
}
/// ODoH and MODoH relay
struct DoHRelay {
  /// authority like "dns.google:443"
  authority: String,
  /// path like "/proxy" that must start from "/"
  path: String,
  /// scheme
  scheme: Scheme,
  /// can be the next hop relay of a client
  can_be_next_hop: bool,
}

/// struct representing a specific path to the target resolver
struct DoHPath {
  /// target resolver
  target: Arc<DoHTarget>,
  /// ordered list of relays, the first one must be flagged as can_be_next_hop
  relays: Vec<Arc<DoHRelay>>,
  /// health flag
  is_healthy: IsHealthy,
  /// doh type
  doh_type: DoHType,
}
impl DoHPath {
  /// build url from the path
  pub fn as_url(&self) -> Result<Url> {
    // standard doh
    match self.doh_type {
      DoHType::Standard => {
        if !self.relays.is_empty() {
          return Err(DapError::FailedToBuildDohUrl);
        }
        let mut url = Url::parse(&self.target.authority)?;
        url.set_scheme(self.target.scheme.as_str()).unwrap();
        url.set_path(&self.target.path);
        Ok(url)
      }
      DoHType::Oblivious => {
        if self.relays.is_empty() || !self.relays[0].can_be_next_hop {
          return Err(DapError::FailedToBuildDohUrl);
        }
        let mut url =
          Url::parse(format!("{}://{}", &self.relays[0].scheme.as_str(), &self.relays[0].authority).as_str())?;
        url.set_path(&self.relays[0].path);
        url
          .query_pairs_mut()
          .append_pair("targethost", self.target.authority.as_str())
          .append_pair("targetpath", self.target.path.as_str());

        // odoh or modoh
        for (idx, relay) in self.relays[1..].iter().enumerate().take(self.relays.len() - 1) {
          url
            .query_pairs_mut()
            .append_pair(format!("relayhost[{}]", idx + 1).as_str(), relay.authority.as_str())
            .append_pair(format!("relaypath[{}]", idx + 1).as_str(), relay.path.as_str());
        }
        Ok(url)
      }
    }
  }
}

/// represents the health of a path
struct IsHealthy(AtomicBool);
impl IsHealthy {
  fn new() -> Self {
    Self(AtomicBool::new(true))
  }
  fn make_halthy(&self) {
    self.0.store(true, Ordering::Relaxed);
  }
  fn make_unhealthy(&self) {
    self.0.store(false, Ordering::Relaxed);
  }
  fn set(&self, is_healthy: bool) {
    self.0.store(is_healthy, Ordering::Relaxed);
  }
  fn get(&self) -> bool {
    self.0.load(Ordering::Relaxed)
  }
}

/// Manages all possible paths
pub struct DoHPathManager {
  /// all possible paths
  /// first dimension: depends on doh target resolver
  /// second dimension: depends on next-hop relays. for the standard doh, its is one dimensional.
  /// third dimension: actual paths. for the standard doh, its is one dimensional.
  paths: Vec<Vec<Vec<Arc<DoHPath>>>>,
  /// target randomization
  target_randomization: bool,
  /// next-hop randomization
  nexthop_randomization: bool,
}
impl DoHPathManager {
  /// build all possible paths
  pub fn new(globals: &Arc<Globals>) -> Result<Self> {
    let targets = globals.proxy_config.target_config.doh_target_urls.iter().map(|url| {
      Arc::new(DoHTarget {
        authority: url.authority().to_string(),
        path: url.path().to_string(),
        scheme: Scheme::try_from(url.scheme()).unwrap_or(Scheme::Https),
      })
    });

    // standard doh
    if globals.proxy_config.nexthop_relay_config.is_none() {
      let paths = targets
        .map(|target| {
          vec![vec![Arc::new(DoHPath {
            target,
            relays: vec![],
            is_healthy: IsHealthy::new(),
            doh_type: DoHType::Standard,
          })]]
        })
        .collect::<Vec<_>>();
      return Ok(Self {
        paths,
        target_randomization: globals.proxy_config.target_config.target_randomization,
        nexthop_randomization: false,
      });
    }

    // odoh and modoh
    let nexthop_relay_config = globals.proxy_config.nexthop_relay_config.as_ref().unwrap();
    let nexthops = nexthop_relay_config.odoh_relay_urls.iter().map(|url| {
      Arc::new(DoHRelay {
        authority: url.authority().to_string(),
        path: url.path().to_string(),
        scheme: Scheme::try_from(url.scheme()).unwrap_or(Scheme::Https),
        can_be_next_hop: true,
      })
    });
    let subseq_relay_config = globals.proxy_config.subseq_relay_config.as_ref();
    let subseq_relay_paths = subseq_relay_config.map(|v| {
      let subseq_relays = v.mid_relay_urls.iter().map(|url| {
        Arc::new(DoHRelay {
          authority: url.authority().to_string(),
          path: url.path().to_string(),
          scheme: Scheme::try_from(url.scheme()).unwrap_or(Scheme::Https),
          can_be_next_hop: false,
        })
      });
      let max = v.max_mid_relays.max(subseq_relays.len());
      let mut paths_after_nexthop = vec![];
      (0..max + 1).for_each(|num| {
        let x: Vec<_> = subseq_relays.clone().permutations(num).collect();
        paths_after_nexthop.extend(x);
      });
      paths_after_nexthop
    });
    let relay_paths = nexthops.clone().map(|nexthop| {
      let relays = match &subseq_relay_paths {
        None => vec![vec![nexthop.clone()]],
        Some(subseq_relay_paths) => subseq_relay_paths
          .iter()
          .map(|subseq_relay_path| {
            let mut relays = vec![nexthop.clone()];
            relays.extend(subseq_relay_path.clone());
            relays
          })
          .collect::<Vec<_>>(),
      };
      relays
    });

    // build path object
    let maybe_looped_path = targets.map(|target| {
      relay_paths
        .clone()
        .map(|relay_path| {
          relay_path
            .iter()
            .map(|relays| {
              Arc::new(DoHPath {
                target: target.clone(),
                relays: relays.clone(),
                is_healthy: IsHealthy::new(),
                doh_type: DoHType::Oblivious,
              })
            })
            .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
    });

    let v = maybe_looped_path.clone().collect::<Vec<_>>();

    // TODO: TODO: TODO: remove loop paths: add check loop function in DoHPath

    Ok(Self {
      paths: vec![],
      target_randomization: true,
      nexthop_randomization: true,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use urlencoding::decode;

  #[tokio::test]
  async fn build_url_works() {
    let target = Arc::new(DoHTarget {
      authority: "dns.google".to_string(),
      path: "/dns-query".to_string(),
      scheme: Scheme::Https,
    });
    let relay1 = Arc::new(DoHRelay {
      authority: "relay1.dns.google".to_string(),
      path: "/proxy".to_string(),
      scheme: Scheme::Https,
      can_be_next_hop: true,
    });
    let relay2 = Arc::new(DoHRelay {
      authority: "relay2.dns.google".to_string(),
      path: "/proxy".to_string(),
      scheme: Scheme::Https,
      can_be_next_hop: false,
    });
    let relay3 = Arc::new(DoHRelay {
      authority: "relay3.dns.google".to_string(),
      path: "/proxy".to_string(),
      scheme: Scheme::Https,
      can_be_next_hop: false,
    });
    let path = Arc::new(DoHPath {
      target,
      relays: vec![relay1, relay2, relay3],
      is_healthy: IsHealthy::new(),
      doh_type: DoHType::Oblivious,
    });
    let url = path.as_url().unwrap();
    let decoded = decode(url.as_str()).unwrap();

    assert_eq!(decoded, "https://relay1.dns.google/proxy?targethost=dns.google&targetpath=/dns-query&relayhost[1]=relay2.dns.google&relaypath[1]=/proxy&relayhost[2]=relay3.dns.google&relaypath[2]=/proxy");
  }
}
