use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
pub enum CounterType {
  TCP,
  UDP
}

#[derive(Debug, Clone, Default)]
pub struct Counter {
  pub cnt_total: Arc<AtomicUsize>,
  pub cnt_udp: Arc<AtomicUsize>,
  pub cnt_tcp: Arc<AtomicUsize>,
}

impl Counter {
  pub fn get_current_total(&self) -> usize {
    self.cnt_total.load(Ordering::Relaxed)
  }

  pub fn get_current(&self, ctype: CounterType) -> usize {
    match ctype {
      CounterType::TCP => {
        self.cnt_tcp.load(Ordering::Relaxed)
      },
      CounterType::UDP => {
        self.cnt_udp.load(Ordering::Relaxed)
      }
    }
  }

  pub fn increment(&self, ctype: CounterType) -> usize {
    self.cnt_total.fetch_add(1, Ordering::Relaxed);
    match ctype {
      CounterType::TCP => {
        self.cnt_tcp.fetch_add(1, Ordering::Relaxed)
      },
      CounterType::UDP => {
        self.cnt_udp.fetch_add(1, Ordering::Relaxed)
      }
    }
  }

  pub fn decrement(&self, ctype: CounterType) {
    let mut cnt;
    match ctype {
      CounterType::TCP => {
        while {
          cnt = self.cnt_tcp.load(Ordering::Relaxed);
          cnt > 0 && self.cnt_tcp.compare_exchange(cnt, cnt - 1, Ordering::Relaxed, Ordering::Relaxed) != Ok(cnt)
        } {}
      },
      CounterType::UDP => {
        while {
          cnt = self.cnt_udp.load(Ordering::Relaxed);
          cnt > 0 && self.cnt_udp.compare_exchange(cnt, cnt - 1, Ordering::Relaxed, Ordering::Relaxed) != Ok(cnt)
        } {}
      }
    };
    self.cnt_total.store(
      self.cnt_udp.load(Ordering::Relaxed) + self.cnt_tcp.load(Ordering::Relaxed),
      Ordering::Relaxed
    );
  }
}
