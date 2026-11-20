use std::{io::{self, Read, Write}, net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream}, sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc}, thread::{self, JoinHandle}, time::Duration};

const UNLINK: &[u8] = b"eow-lite unlink\n";
const STOPPED: &[u8] = b"eow-lite stopped\n";
const STOP_FAILED: &[u8] = b"eow-lite failed!\n";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Control {
    done: mpsc::Sender<bool>,
    thread: JoinHandle<()>,
}

impl Control {

    pub fn bind(port: u16, stopping: Arc<AtomicBool>) -> io::Result<Self> {
        let listener: TcpListener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|err: io::Error| {
            io::Error::new(
                err.kind(),
                format!("cannot bind Supervisor control port {port}: {err}"),
            )
        })?;
        listener.set_nonblocking(true)?;
        let (done, finished): (mpsc::Sender<bool>, mpsc::Receiver<bool>) = mpsc::channel();
        let thread: JoinHandle<()> = thread::spawn(move || serve(listener, stopping, finished));
        Ok(Self { done, thread })
    }

    pub fn finish(self, clean: bool) {
        let _ = self.done.send(clean);
        let _ = self.thread.join();
    }

}

fn serve(listener: TcpListener, stopping: Arc<AtomicBool>, finished: mpsc::Receiver<bool>) {
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Err(err) = stream
                    .set_nonblocking(false)
                    .and_then(|_| stream.set_read_timeout(Some(CONTROL_TIMEOUT)))
                    .and_then(|_| stream.set_write_timeout(Some(CONTROL_TIMEOUT)))
                {
                    eprintln!("supervisor control: {err}");
                    continue;
                }
                let mut request: [u8; UNLINK.len()] = [0; UNLINK.len()];
                if stream.read_exact(&mut request).is_err() || request != UNLINK {
                    continue;
                }
                stopping.store(true, Ordering::Release);
                let clean: bool = finished.recv().unwrap_or(false);
                drop(listener);
                let _ = stream.write_all(if clean { STOPPED } else { STOP_FAILED });
                return;
            },
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if !matches!(
                    finished.recv_timeout(Duration::from_millis(20)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    return;
                }
            },
            Err(err) => {
                eprintln!("supervisor control: {err}");
                stopping.store(true, Ordering::Release);
                return;
            },
        }
    }
}

pub fn unlink(port: u16, timeout: Duration) -> io::Result<()> {
    let address: SocketAddr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream: TcpStream = TcpStream::connect_timeout(&address, CONTROL_TIMEOUT).map_err(|err: io::Error| {
        io::Error::new(
            err.kind(),
            format!("cannot reach EOW Lite Supervisor at {address}: {err}"),
        )
    })?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(UNLINK)?;
    let mut reply: [u8; STOPPED.len()] = [0; STOPPED.len()];
    stream.read_exact(&mut reply)?;
    if reply != STOPPED {
        return Err(io::Error::other(
            "Supervisor did not confirm that all modules stopped",
        ));
    }
    Ok(())
}
