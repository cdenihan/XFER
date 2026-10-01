//! Cooperative cancellation for transfer workers, including blocked socket I/O.
use std::{
    io,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::error::{Result, XferError};

/// Use one control per transfer. Cancellation is permanent; create a fresh
/// control for a retry. Source planning checks cancellation between entries. Name resolution must
/// return before cancellation can be observed; attached network I/O is interrupted immediately.
#[derive(Default, Clone)]
pub struct TransferControl {
    state: Arc<TransferControlState>,
}

#[derive(Default)]
struct TransferControlState {
    cancelled: AtomicBool,
    stream: Mutex<Option<TcpStream>>,
}

impl TransferControl {
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
        if let Some(stream) = self
            .state
            .stream
            .lock()
            .expect("transfer control mutex")
            .as_ref()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn check(&self) -> Result<()> {
        if self.state.cancelled.load(Ordering::SeqCst) {
            Err(XferError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(crate) fn attach(&self, stream: &TcpStream) -> Result<()> {
        let mut active = self.state.stream.lock().expect("transfer control mutex");
        self.check()?;
        *active = Some(stream.try_clone()?);
        Ok(())
    }

    pub(crate) fn finish<T>(&self, result: Result<T>) -> Result<T> {
        self.state
            .stream
            .lock()
            .expect("transfer control mutex")
            .take();
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

/// Socket I/O that observes cancellation even when Windows keeps a blocking
/// receive pending after shutdown through a duplicated socket handle.
pub(crate) struct ControlledStream {
    stream: TcpStream,
    control: TransferControl,
}
impl ControlledStream {
    pub(crate) fn new(stream: TcpStream, control: &TransferControl) -> Self {
        Self {
            stream,
            control: control.clone(),
        }
    }
    fn check_io(&self) -> io::Result<()> {
        self.control
            .check()
            .map_err(|error| io::Error::new(io::ErrorKind::ConnectionAborted, error))
    }
    #[cfg(windows)]
    fn poll_io(
        &mut self,
        reading: bool,
        mut operation: impl FnMut(&mut TcpStream) -> io::Result<usize>,
    ) -> io::Result<usize> {
        let timeout = if reading {
            self.stream.read_timeout()?
        } else {
            self.stream.write_timeout()?
        };
        let started = std::time::Instant::now();
        let result = (|| loop {
            self.check_io()?;
            let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "socket I/O timed out",
                ));
            }
            let interval = remaining
                .unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100));
            if reading {
                self.stream.set_read_timeout(Some(interval))?;
            } else {
                self.stream.set_write_timeout(Some(interval))?;
            }
            match operation(&mut self.stream) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                result => return result,
            }
        })();
        // Preserve the caller's timeout policy, including indefinite trust waits.
        let restored = if reading {
            self.stream.set_read_timeout(timeout)
        } else {
            self.stream.set_write_timeout(timeout)
        };
        match result {
            Ok(count) => {
                restored?;
                Ok(count)
            }
            Err(error) => Err(error),
        }
    }
}
impl std::ops::Deref for ControlledStream {
    type Target = TcpStream;
    fn deref(&self) -> &TcpStream {
        &self.stream
    }
}
impl io::Read for ControlledStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            self.poll_io(true, |stream| io::Read::read(stream, buffer))
        }
        #[cfg(not(windows))]
        {
            self.check_io()?;
            io::Read::read(&mut self.stream, buffer)
        }
    }
}
impl io::Write for ControlledStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            self.poll_io(false, |stream| io::Write::write(stream, buffer))
        }
        #[cfg(not(windows))]
        {
            self.check_io()?;
            io::Write::write(&mut self.stream, buffer)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.check_io()?;
        io::Write::flush(&mut self.stream)
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
    fn configured_blocking_socket_observes_cancellation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        crate::net::configure_stream(&server).unwrap();
        let control = Arc::new(TransferControl::default());
        control.attach(&server).unwrap();
        let worker_control = Arc::clone(&control);
        let (started, waiting) = mpsc::channel();
        let (done, result) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut stream = ControlledStream::new(server, &worker_control);
            started.send(()).unwrap();
            done.send(stream.read(&mut [0])).unwrap();
        });
        waiting.recv().unwrap();
        thread::sleep(Duration::from_millis(150));
        control.cancel();
        let read = result.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(read.is_err() || read.unwrap() == 0);
        worker.join().unwrap();
        drop(client);
    }
    #[test]
    fn controlled_socket_preserves_read_timeout() {
        use std::io::Write;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let control = TransferControl::default();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        server.write_all(b"x").unwrap();
        let mut stream = ControlledStream::new(client, &control);
        let mut buffer = [0];
        stream.read_exact(&mut buffer).unwrap();
        assert_eq!(buffer, *b"x");
        assert_eq!(stream.read_timeout().unwrap(), Some(Duration::from_secs(3)));
    }
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
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let control = TransferControl::default();
        control.attach(&client).unwrap();
        let (done, result) = mpsc::channel();
        let worker_control = control.clone();
        let worker = thread::spawn(move || {
            let mut client = ControlledStream::new(client, &worker_control);
            done.send(client.read(&mut [0])).unwrap();
        });
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
