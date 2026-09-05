//! Cooperative cancellation for transfer workers, including blocked socket I/O.
use std::{
    io,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::error::{Result, XferError};

/// Use one control per transfer. Cancellation is permanent; create a fresh
/// control for a retry. Name resolution and source planning finish before
/// cancellation can be observed; attached network I/O is interrupted immediately.
#[derive(Default)]
pub struct TransferControl {
    cancelled: AtomicBool,
    stream: Mutex<Option<TcpStream>>,
}

impl TransferControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(stream) = self.stream.lock().expect("transfer control mutex").as_ref() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(XferError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn attach(&self, stream: &TcpStream) -> Result<()> {
        let mut active = self.stream.lock().expect("transfer control mutex");
        self.check()?;
        *active = Some(stream.try_clone()?);
        Ok(())
    }

    pub(crate) fn finish<T>(&self, result: Result<T>) -> Result<T> {
        self.stream.lock().expect("transfer control mutex").take();
        // Preserve completed transfers if cancellation arrives after installation.
        if result.is_err() {
            self.check()?;
        }
        result
    }

    pub(crate) fn accept(&self, listener: &TcpListener) -> Result<(TcpStream, SocketAddr)> {
        listener.set_nonblocking(true)?;
        let result = loop {
            if let Err(error) = self.check() {
                break Err(error);
            }
            match listener.accept() {
                Ok(connection) => break Ok(connection),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => break Err(error.into()),
            }
        };
        listener.set_nonblocking(false)?;
        let (stream, peer) = result?;
        stream.set_nonblocking(false)?;
        self.attach(&stream)?;
        Ok((stream, peer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        sync::{Arc, mpsc},
    };

    #[test]
    fn cancellation_releases_a_waiting_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let control = Arc::new(TransferControl::default());
        let worker_control = Arc::clone(&control);
        let (done, result) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(worker_control.accept(&listener)).unwrap();
        });
        control.cancel();
        assert!(matches!(
            result.recv_timeout(Duration::from_secs(2)).unwrap(),
            Err(XferError::Cancelled)
        ));
        worker.join().unwrap();
    }

    #[test]
    fn cancellation_interrupts_blocked_reads() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let control = TransferControl::default();
        control.attach(&client).unwrap();
        let (done, result) = mpsc::channel();
        let worker = thread::spawn(move || done.send(client.read(&mut [0])).unwrap());
        control.cancel();
        let read = result.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(read.is_err() || read.unwrap() == 0);
        worker.join().unwrap();
        assert!(matches!(
            control.finish::<()>(Err(io::Error::other("closed").into())),
            Err(XferError::Cancelled)
        ));
    }
}
