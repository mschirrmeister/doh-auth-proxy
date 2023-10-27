use super::{
  cache::Cache,
  dns_message::{self, Request},
  odoh_config_store::ODoHConfigStore,
  path_manage::DoHPathManager,
  DoHMethod, DoHType,
};
use crate::{
  auth::Authenticator,
  error::*,
  globals::Globals,
  http_client::HttpClientInner,
  log::*,
  trait_resolve_ips::{ResolveIpResponse, ResolveIps},
};
use async_trait::async_trait;
use data_encoding::BASE64URL_NOPAD;
use reqwest::header;
use std::sync::Arc;
use tokio::sync::RwLock;
use url::Url;

/// DoH, ODoH, MODoH client
pub struct DoHClient {
  /// http client to make doh query
  http_client: Arc<RwLock<HttpClientInner>>,
  /// auth_client to retrieve id token
  auth_client: Option<Arc<Authenticator>>,
  /// path candidates with health flags
  path_manager: Arc<DoHPathManager>,
  /// odoh config store
  odoh_configs: Option<Arc<ODoHConfigStore>>,
  /// DNS cache
  cache: Arc<Cache>,
  /// DoH type
  doh_type: DoHType,
  /// DoH method
  doh_method: DoHMethod,
  /// base headers
  headers: header::HeaderMap,
}

impl DoHClient {
  /// Create a new DoH client
  pub async fn new(
    globals: Arc<Globals>,
    http_client: Arc<RwLock<HttpClientInner>>,
    auth_client: Option<Arc<Authenticator>>,
  ) -> Result<Self> {
    // 1. build all path candidates from globals
    let path_manager = Arc::new(DoHPathManager::new(&globals)?);

    // 2. spawn odoh config service if odoh or modoh are enabled
    let odoh_configs = match &globals.proxy_config.nexthop_relay_config {
      Some(nexthop_relay_config) => {
        if nexthop_relay_config.odoh_relay_urls.is_empty() {
          return Err(DapError::ODoHNoRelayUrl);
        }
        let odoh_configs = Arc::new(ODoHConfigStore::new(http_client.clone(), &path_manager.targets()).await?);
        let odoh_config_clone = odoh_configs.clone();
        let term_notify = globals.term_notify.clone();
        globals
          .runtime_handle
          .spawn(async move { odoh_config_clone.start_service(term_notify).await });
        Some(odoh_configs)
      }
      None => None,
    };

    // doh type
    let doh_type = match &globals.proxy_config.nexthop_relay_config {
      Some(nexthop_relay_config) => {
        if nexthop_relay_config.odoh_relay_urls.is_empty() {
          DoHType::Standard
        } else {
          DoHType::Oblivious
        }
      }
      None => DoHType::Standard,
    };
    // base headers except for authorization
    let mut headers = header::HeaderMap::new();
    let ct = doh_type.as_str();
    headers.insert("Accept", header::HeaderValue::from_str(&ct).unwrap());
    headers.insert("Content-Type", header::HeaderValue::from_str(&ct).unwrap());
    if let DoHType::Oblivious = doh_type {
      headers.insert(
        "Cache-Control",
        header::HeaderValue::from_str("no-cache, no-store").unwrap(),
      );
    }

    // doh method
    let doh_method = match doh_type {
      DoHType::Standard => globals.proxy_config.target_config.doh_method.clone(),
      DoHType::Oblivious => DoHMethod::Post,
    };

    // cache
    let cache = Arc::new(Cache::new(globals.proxy_config.max_cache_size));

    // TODO: 3. spawn healthcheck for every possible path? too many?
    // TODO: 4. cache purge service, simultaneously with healthcheck?
    Ok(Self {
      http_client,
      auth_client,
      path_manager,
      odoh_configs,
      cache,
      doh_type,
      doh_method,
      headers,
    })
  }

  /// Make DoH query
  pub async fn make_doh_query(&self, packet_buf: &[u8]) -> Result<Vec<u8>> {
    // Check if the given packet buffer is consistent as a DNS query
    let query_msg = dns_message::is_query(packet_buf).map_err(|e| {
      error!("{e}");
      DapError::InvalidDnsQuery
    })?;
    // TODO: If error, should we build and return a synthetic reject response message?
    let query_id = query_msg.id();
    let req = Request::try_from(&query_msg).map_err(|e| {
      error!("Failed to parse DNS query, maybe invalid DNS query: {e}");
      DapError::InvalidDnsQuery
    })?;

    // // Process query plugins, e.g., domain filtering, cloaking, etc.
    // if let Some(query_plugins) = context.query_plugins.clone() {
    //   let execution_result = query_plugins.execute(&query_msg, &req.0[0], context.min_ttl)?;
    //   match execution_result.action {
    //     plugins::QueryPluginAction::Pass => (),
    //     _ => {
    //       // plugins::QueryPluginsAction::Blocked or Overridden
    //       if let Some(r_msg) = execution_result.response_msg {
    //         let res = dns_message::encode(&r_msg)?;
    //         return Ok(res);
    //       } else {
    //         bail!("Invalid response message by query plugins");
    //       }
    //     }
    //   }
    // }

    // Check cache and return if hit
    if let Some(res) = self.cache.get(&req).await {
      debug!("Cache hit!: {:?}", res.message().queries());
      if let Ok(response_buf) = res.build_response(query_id) {
        return Ok(response_buf);
      } else {
        error!("Cached object is somewhat invalid");
      }
    }

    let response_result = match self.doh_type {
      DoHType::Standard => self.serve_doh_query(packet_buf).await,
      DoHType::Oblivious => self.serve_oblivious_doh_query(packet_buf).await,
    };

    match response_result {
      Ok(response_buf) => {
        // Check if the returned packet buffer is consistent as a DNS response
        // TODO: If error, should we build and return a synthetic reject response message?
        let response_message = dns_message::is_response(&response_buf).map_err(|e| {
          error!("{e}");
          DapError::InvalidDnsResponse
        })?;

        if (self.cache.put(req, &response_message).await).is_err() {
          error!("Failed to cache a DNS response");
        };
        // TODO: should rebuild buffer from decoded dns response_msg?
        Ok(response_buf)
      }
      Err(e) => Err(e),
    }
  }

  //// build headers for doh and odoh query with authorization if needed
  async fn build_headers(&self) -> Result<header::HeaderMap> {
    let mut headers = self.headers.clone();
    match &self.auth_client {
      Some(auth) => {
        debug!("build headers with http authorization header");
        let token = auth.id_token().await?;
        let token_str = format!("Bearer {}", &token);
        headers.insert(
          header::AUTHORIZATION,
          header::HeaderValue::from_str(&token_str).unwrap(),
        );
        todo!()
      }
      None => Ok(headers),
    }
  }

  /// serve doh query
  async fn serve_doh_query(&self, packet_buf: &[u8]) -> Result<Vec<u8>> {
    let Some(target_url) = self.path_manager.get_path() else {
      return Err(DapError::NoPathAvailable);
    };
    let target_url = target_url.as_url()?;
    let headers = self.build_headers().await?;

    let response = match &self.doh_method {
      DoHMethod::Get => {
        let query_b64u = BASE64URL_NOPAD.encode(packet_buf);
        let query_url = format!("{}?dns={}", target_url.as_str(), query_b64u);
        debug!("query url: {:?}", query_url);
        let lock = self.http_client.read().await;
        lock.get(query_url).headers(headers).send().await?
      }
      DoHMethod::Post => {
        let lock = self.http_client.read().await;
        lock
          .post(target_url)
          .headers(headers)
          .body(packet_buf.to_owned())
          .send()
          .await?
      }
    };

    if response.status() != reqwest::StatusCode::OK {
      error!("DoH query error!: {:?}", response.status());
      return Err(DapError::DoHQueryError);
    }

    let body = response.bytes().await?;
    Ok(body.to_vec())
  }

  /// serve oblivious doh query
  async fn serve_oblivious_doh_query(&self, packet_buf: &[u8]) -> Result<Vec<u8>> {
    let Some(target_url) = self.path_manager.get_path() else {
      return Err(DapError::NoPathAvailable);
    };
    let target_url = target_url.as_url()?;
    let headers = self.build_headers().await?;

    // odoh config
    if self.odoh_configs.is_none() {
      return Err(DapError::ODoHNoClientConfig);
    }

    todo!()
  }
}

// TODO: implement ResolveIps for DoHClient
#[async_trait]
impl ResolveIps for Arc<DoHClient> {
  /// Resolve ip addresses of the given domain name
  async fn resolve_ips(&self, domain: &Url) -> Result<ResolveIpResponse> {
    Err(DapError::Other(anyhow!("Not implemented")))
  }
}
