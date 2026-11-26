//! Glue between flutter_rust_bridge types and the `sync-ffi` callback
//! traits. Kept outside `crate::api` so the code generator does not export
//! these helpers to Dart.

use crate::frb_generated::StreamSink;
use flutter_rust_bridge::DartFnFuture;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use sync_ffi::{FfiError, FfiEvent, FfiListener, FfiRows, FfiStatus, FfiTokenProvider, FfiWatcher};

pub(crate) type DartTokenFn = dyn Fn(bool) -> DartFnFuture<String> + Send + Sync;

/// Calls the Dart token callback and waits for it. This runs on a bridge
/// worker thread, never on the Dart thread, so waiting cannot deadlock.
pub(crate) struct DartTokens(pub(crate) Box<DartTokenFn>);

impl FfiTokenProvider for DartTokens {
    fn token(&self, force_refresh: bool) -> Result<String, String> {
        let token = futures::executor::block_on((self.0)(force_refresh));
        if token.is_empty() {
            Err("no token".into())
        } else {
            Ok(token)
        }
    }
}

/// A watch or listener whose Dart stream has been cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dead {
    Watch(u64),
    Listener(u64),
}

/// Collects registrations whose Dart side has gone away, so the engine can
/// drop them. When a Dart stream is cancelled, the bridge closes its port
/// and the next send from Rust fails; that failure is the only signal Rust
/// gets. Registrations are removed on the next call into the engine rather
/// than from inside the callback.
#[derive(Default)]
pub(crate) struct Reaper(Mutex<Vec<Dead>>);

impl Reaper {
    pub(crate) fn take(&self) -> Vec<Dead> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    fn push(&self, d: Dead) {
        self.0.lock().unwrap().push(d);
    }
}

/// The registration ID and closed state shared by every sink. The ID is
/// only known after registering, and a send may fail before that (a watch
/// emits its first result during registration), so whichever of the two
/// happens second reports the registration as dead.
struct Registration {
    id: OnceLock<u64>,
    closed: AtomicBool,
    reaper: Arc<Reaper>,
    kind: fn(u64) -> Dead,
}

impl Registration {
    fn new(reaper: Arc<Reaper>, kind: fn(u64) -> Dead) -> Self {
        Registration {
            id: OnceLock::new(),
            closed: AtomicBool::new(false),
            reaper,
            kind,
        }
    }

    fn set_id(&self, id: u64) {
        let _ = self.id.set(id);
        if self.closed.load(Ordering::SeqCst) {
            self.reaper.push((self.kind)(id));
        }
    }

    fn mark_closed(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            if let Some(id) = self.id.get() {
                self.reaper.push((self.kind)(*id));
            }
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

enum ListenerSink {
    Status(Mutex<StreamSink<FfiStatus>>),
    Events(Mutex<StreamSink<FfiEvent>>),
    Tables(Mutex<StreamSink<Vec<String>>>),
}

/// Forwards one kind of engine notification to one Dart stream.
pub(crate) struct Listener {
    sink: ListenerSink,
    reg: Registration,
}

impl Listener {
    fn new(sink: ListenerSink, reaper: Arc<Reaper>) -> Arc<Self> {
        Arc::new(Listener {
            sink,
            reg: Registration::new(reaper, Dead::Listener),
        })
    }

    pub(crate) fn status(sink: StreamSink<FfiStatus>, reaper: Arc<Reaper>) -> Arc<Self> {
        Self::new(ListenerSink::Status(Mutex::new(sink)), reaper)
    }

    pub(crate) fn events(sink: StreamSink<FfiEvent>, reaper: Arc<Reaper>) -> Arc<Self> {
        Self::new(ListenerSink::Events(Mutex::new(sink)), reaper)
    }

    pub(crate) fn tables(sink: StreamSink<Vec<String>>, reaper: Arc<Reaper>) -> Arc<Self> {
        Self::new(ListenerSink::Tables(Mutex::new(sink)), reaper)
    }

    pub(crate) fn set_id(&self, id: u64) {
        self.reg.set_id(id);
    }

    fn sent(&self, ok: bool) {
        if !ok {
            self.reg.mark_closed();
        }
    }
}

impl FfiListener for Listener {
    fn on_status(&self, s: FfiStatus) {
        if let ListenerSink::Status(sink) = &self.sink {
            if !self.reg.is_closed() {
                self.sent(sink.lock().unwrap().add(s).is_ok());
            }
        }
    }

    fn on_event(&self, e: FfiEvent) {
        if let ListenerSink::Events(sink) = &self.sink {
            if !self.reg.is_closed() {
                self.sent(sink.lock().unwrap().add(e).is_ok());
            }
        }
    }

    fn on_tables_changed(&self, t: Vec<String>) {
        if let ListenerSink::Tables(sink) = &self.sink {
            if !self.reg.is_closed() {
                self.sent(sink.lock().unwrap().add(t).is_ok());
            }
        }
    }
}

/// Forwards one watch query's results to its Dart stream.
pub(crate) struct RowsSink {
    sink: Mutex<StreamSink<FfiRows>>,
    reg: Registration,
}

impl RowsSink {
    pub(crate) fn new(sink: StreamSink<FfiRows>, reaper: Arc<Reaper>) -> Arc<Self> {
        Arc::new(RowsSink {
            sink: Mutex::new(sink),
            reg: Registration::new(reaper, Dead::Watch),
        })
    }

    pub(crate) fn set_id(&self, id: u64) {
        self.reg.set_id(id);
    }
}

impl FfiWatcher for RowsSink {
    fn on_rows(&self, rows: FfiRows) {
        if self.reg.is_closed() {
            return;
        }
        if self.sink.lock().unwrap().add(rows).is_err() {
            self.reg.mark_closed();
        }
    }

    fn on_error(&self, message: String) {
        if self.reg.is_closed() {
            return;
        }
        let err = FfiError {
            code: "database".into(),
            message,
        };
        if self.sink.lock().unwrap().add_error(err).is_err() {
            self.reg.mark_closed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_before_id_is_reported_once_id_is_known() {
        let reaper = Arc::new(Reaper::default());
        let reg = Registration::new(reaper.clone(), Dead::Watch);
        reg.mark_closed();
        assert!(reaper.take().is_empty());
        reg.set_id(7);
        assert_eq!(reaper.take(), vec![Dead::Watch(7)]);
    }

    #[test]
    fn closed_after_id_is_reported_once() {
        let reaper = Arc::new(Reaper::default());
        let reg = Registration::new(reaper.clone(), Dead::Listener);
        reg.set_id(3);
        reg.mark_closed();
        reg.mark_closed();
        assert_eq!(reaper.take(), vec![Dead::Listener(3)]);
        assert!(reaper.take().is_empty());
    }
}
