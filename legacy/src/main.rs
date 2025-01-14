#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod client;
mod config;
mod constants;
mod context;
mod dns_message;
mod error;
mod log;
mod plugins;
mod proxy;
mod servers;
use crate::{config::parse_opts, log::*, proxy::Proxy};

fn main() {
  init_logger();
  info!("Start DoH w/ Auth Proxy");

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name("doh-auth-proxy");
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    let context = match parse_opts(runtime.handle()).await {
      Ok(g) => g,
      Err(e) => {
        error!("Failed to parse config TOML: {}", e);
        std::process::exit(1);
      }
    };

    let proxy = Proxy { context };
    proxy.entrypoint().await.unwrap()
  });
}
