//! One bounded command queue and owner thread; callbacks never run on that thread.
use crate::{DeviceConfig, DeviceCore, DeviceError as Error, Result};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
};

pub trait DeviceListener: Send + Sync {
    /// Full invalidation, no clinical data. Fetch current views; revisions can coalesce.
    fn invalidated(&self, revision: u64);
}
type Work = Box<dyn FnOnce(&mut DeviceCore) -> bool + Send>;
enum Command {
    Run(Work),
    Close(mpsc::Sender<Result<()>>),
}
const OPEN: u8 = 0;
const CLOSING: u8 = 1;
const CLOSED: u8 = 2;
const FAULTED: u8 = 3;

pub struct DeviceHandle {
    admission: Mutex<SyncSender<Command>>,
    status: Arc<AtomicU8>,
    owner: Mutex<Option<JoinHandle<()>>>,
    dispatcher: Mutex<Option<JoinHandle<()>>>,
    dispatcher_id: thread::ThreadId,
    listener: Arc<Mutex<Option<Arc<dyn DeviceListener>>>>,
    notifications: SyncSender<()>,
    revision: Arc<AtomicU64>,
    stop_callbacks: Arc<AtomicBool>,
    closing: Mutex<()>,
}
impl DeviceHandle {
    pub fn open(config: DeviceConfig) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Command>(64);
        let (ready_tx, ready_rx) = mpsc::channel();
        let status = Arc::new(AtomicU8::new(OPEN));
        let thread_status = status.clone();
        let owner = thread::Builder::new()
            .name("er-device-commands".into())
            .spawn(move || {
                let opened = catch_unwind(AssertUnwindSafe(|| DeviceCore::open(config)));
                let mut core = match opened {
                    Ok(Ok(core)) => {
                        let _ = ready_tx.send(Ok(()));
                        core
                    }
                    Ok(Err(e)) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                    Err(payload) => {
                        std::mem::forget(payload);
                        let _ = ready_tx.send(Err(Error::Internal));
                        return;
                    }
                };
                while let Ok(command) = rx.recv() {
                    match command {
                        Command::Run(work) => {
                            if !work(&mut core) {
                                thread_status.store(FAULTED, Ordering::Release);
                                break;
                            }
                        }
                        Command::Close(reply) => {
                            let result = match catch_unwind(AssertUnwindSafe(|| core.close())) {
                                Ok(r) => r,
                                Err(p) => {
                                    std::mem::forget(p);
                                    Err(Error::Internal)
                                }
                            };
                            // Drop runtime/SQLite/ownership on THIS thread before close returns.
                            drop(core);
                            thread_status.store(CLOSED, Ordering::Release);
                            let _ = reply.send(result);
                            return;
                        }
                    }
                }
                drop(core);
            })
            .map_err(|_| Error::Internal)?;
        if let Err(e) = ready_rx.recv().unwrap_or(Err(Error::Internal)) {
            let _ = owner.join();
            return Err(e);
        }
        let (notify_tx, notify_rx) = mpsc::sync_channel::<()>(1);
        let listener: Arc<Mutex<Option<Arc<dyn DeviceListener>>>> = Arc::new(Mutex::new(None));
        let thread_listener = listener.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let revision = Arc::new(AtomicU64::new(0));
        let thread_revision = revision.clone();
        let dispatcher = thread::Builder::new()
            .name("er-device-notifications".into())
            .spawn(move || {
                while notify_rx.recv().is_ok() {
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let callback = thread_listener.lock().unwrap().clone();
                    if let Some(callback) = callback {
                        // Foreign listener failures do not poison clinical state.
                        if let Err(p) = catch_unwind(AssertUnwindSafe(|| {
                            callback.invalidated(thread_revision.load(Ordering::Acquire))
                        })) {
                            std::mem::forget(p);
                        }
                    }
                }
            })
            .map_err(|_| Error::Internal)?;
        Ok(Self {
            admission: Mutex::new(tx),
            status,
            owner: Mutex::new(Some(owner)),
            dispatcher_id: dispatcher.thread().id(),
            dispatcher: Mutex::new(Some(dispatcher)),
            listener,
            notifications: notify_tx,
            revision,
            stop_callbacks: stop,
            closing: Mutex::new(()),
        })
    }
    /// Once admitted there is no timeout: caller receives the actual result.
    pub fn execute<T: Send + 'static>(
        &self,
        mutation: bool,
        operation: impl FnOnce(&mut DeviceCore) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let notify = self.notifications.clone();
        let revision = self.revision.clone();
        let work = Box::new(move |core: &mut DeviceCore| {
            match catch_unwind(AssertUnwindSafe(|| operation(core))) {
                Ok(result) => {
                    // Also invalidate on a failed mutation: a safety stop may have
                    // succeeded before a later metadata write failed.
                    if mutation {
                        revision.fetch_add(1, Ordering::AcqRel);
                        let _ = notify.try_send(());
                    }
                    let _ = reply_tx.send(result);
                    true
                }
                Err(payload) => {
                    std::mem::forget(payload);
                    let _ = reply_tx.send(Err(Error::Internal));
                    false
                }
            }
        });
        {
            let admission = self.admission.lock().unwrap();
            match self.status.load(Ordering::Acquire) {
                OPEN => {}
                FAULTED => return Err(Error::Internal),
                _ => return Err(Error::Closed),
            }
            admission
                .try_send(Command::Run(work))
                .map_err(|e| match e {
                    mpsc::TrySendError::Full(_) => Error::Busy,
                    mpsc::TrySendError::Disconnected(_) => Error::Internal,
                })?;
        }
        reply_rx.recv().unwrap_or(Err(Error::Internal))
    }
    pub fn set_listener(&self, listener: Option<Arc<dyn DeviceListener>>) -> Result<()> {
        let _admission = self.admission.lock().unwrap();
        if self.status.load(Ordering::Acquire) != OPEN {
            return Err(Error::Closed);
        }
        *self.listener.lock().unwrap() = listener;
        let _ = self.notifications.try_send(());
        Ok(())
    }
    pub fn close(&self) -> Result<()> {
        // A callback may call close while another close is waiting to join it.
        // Such a reentrant close returns immediately once admission has stopped.
        let on_dispatcher = self.dispatcher_id == thread::current().id();
        if on_dispatcher && self.status.load(Ordering::Acquire) != OPEN {
            return Ok(());
        }
        let _closing = if on_dispatcher {
            // Another closer can be joining THIS callback. Never wait for its
            // close lock, even if it acquired that lock just before setting status.
            match self.closing.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
                Err(std::sync::TryLockError::Poisoned(_)) => return Err(Error::Internal),
            }
        } else {
            self.closing.lock().unwrap()
        };
        let response = {
            let admission = self.admission.lock().unwrap();
            match self.status.load(Ordering::Acquire) {
                OPEN => {
                    self.status.store(CLOSING, Ordering::Release);
                    let (tx, rx) = mpsc::channel();
                    admission
                        .send(Command::Close(tx))
                        .map_err(|_| Error::Internal)?;
                    Some(rx)
                }
                _ => None,
            }
        };
        let result = response.map_or(Ok(()), |rx| rx.recv().unwrap_or(Err(Error::Internal)));
        if let Some(owner) = self.owner.lock().unwrap().take() {
            let _ = owner.join();
        }
        self.status.store(CLOSED, Ordering::Release);
        self.stop_callbacks.store(true, Ordering::Release);
        *self.listener.lock().unwrap() = None;
        let _ = self.notifications.try_send(());
        let dispatcher = self.dispatcher.lock().unwrap().take();
        if let Some(dispatcher) = dispatcher
            && dispatcher.thread().id() != thread::current().id()
        {
            let _ = dispatcher.join();
        }
        result
    }
}
impl Drop for DeviceHandle {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
