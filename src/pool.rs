use std::net::SocketAddr;
use std::collections::VecDeque;
use std::rc::Rc;
use std::cell::RefCell;
use std::sync::Arc;

use futures::prelude::*;
use tokio_core::net::TcpStream;
use tokio_core::reactor::Handle;
use bytes::Bytes;
use h2::{self, client as h2c};
use http::Request;
use rustls::{self, Session};
use tokio_rustls::{TlsStream, ClientConfigExt};

use super::ALPN_H2;

#[derive(Clone)]
pub struct PoolHandle {
    domain: Rc<String>,
    addr: SocketAddr,
    tls_config: Arc<rustls::ClientConfig>,
    handle: Handle,
    task: Rc<RefCell<Option<::futures::task::Task>>>,
    pool: Rc<RefCell<VecDeque<h2c::Client<TlsStream<TcpStream, rustls::ClientSession>, Bytes>>>>,
}

#[derive(Clone)]
pub struct H2ClientPool(PoolHandle);

impl H2ClientPool {
    pub fn new(
        handle: Handle,
        tls_config: Arc<rustls::ClientConfig>,
        domain: String,
        addr: SocketAddr,
    ) -> H2ClientPool {
        let h = PoolHandle {
            domain: Rc::new(domain),
            addr: addr,
            handle: handle,
            tls_config: tls_config,
            task: Rc::new(RefCell::new(None)),
            pool: Rc::new(RefCell::new(VecDeque::new())),
        };
        H2ClientPool(h)
    }

    pub fn handle(&self) -> PoolHandle {
        self.0.clone()
    }
}

impl Future for H2ClientPool {
    type Item = ();
    type Error = ();
    fn poll(&mut self) -> Poll<(), ()> {
        if self.0.task.borrow().is_none() {
            *self.0.task.borrow_mut() = Some(::futures::task::current());
        }

        if Rc::strong_count(&self.0.pool) == 1 {
            let wired_strems: usize = self.0
                .pool
                .borrow()
                .iter()
                .map(h2c::Client::num_wired_streams)
                .sum();
            if wired_strems == 0 {
                // free memory
                return Ok(Async::Ready(()));
            }
        }

        let len = self.0.pool.borrow().len();
        for idx in 0..len {
            let mut pool = self.0.pool.borrow_mut();
            let remove = {
                let client = pool.get_mut(idx).unwrap();
                match client.poll() {
                    Ok(Async::Ready(())) => true,
                    Ok(Async::NotReady) => false,
                    Err(e) => {
                        eprintln!("{:?}", e);
                        true
                    }
                }
            };
            if remove {
                pool.remove(idx);
            }
        }
        Ok(Async::NotReady)
    }
}

impl PoolHandle {
    pub fn send_request<'a>(
        &self,
        request: Request<()>,
        end_of_stream: bool,
    ) -> impl Future<Item = h2c::Stream<Bytes>, Error = h2::Error> + 'a {
        let s = self.clone();
        async_block! {
            let mut client = await!(s.pop())?;
            let stream = client.send_request(request, end_of_stream)?;
            s.pool.borrow_mut().push_back(client);
            Ok(stream)
        }
    }

    fn new_client<'a>(
        &self,
    ) -> impl Future<
        Item = h2c::Client<TlsStream<TcpStream, rustls::ClientSession>, Bytes>,
        Error = h2::Error,
    >
                 + 'a {
        let task = self.task.clone();
        let domain = self.domain.clone();
        let tls_config = self.tls_config.clone();
        TcpStream::connect(&self.addr, &self.handle)
            .map_err(h2::Error::from)
            .and_then(move |tcp| {
                tls_config.connect_async(&domain, tcp).map_err(Into::into)
            })
            .and_then(move |socket| {
                let negotiated_protcol = {
                    let (_, session) = socket.get_ref();
                    session.get_alpn_protocol()
                };
                if let Some(ALPN_H2) = negotiated_protcol.as_ref().map(|x| &**x) {
                } else {
                    println!("not a http2 server!");
                }
                if let Some(ref task) = *task.borrow() {
                    task.notify();
                }
                h2c::Client::handshake(socket)
            })
    }

    fn pop<'a>(
        &self,
    ) -> impl Future<
        Item = h2c::Client<TlsStream<TcpStream, rustls::ClientSession>, Bytes>,
        Error = h2::Error,
    >
                 + 'a {
        let s = self.clone();
        async_block!{
            let client = s.pool.borrow_mut().pop_front();
            let mut client = match client {
                Some(x) => x,
                None => await!(s.new_client())?,
            };

            if client.poll_ready().unwrap().is_not_ready() {
                unimplemented!() //await!(s.pop())
            } else {
                Ok(client)
            }
        }
    }
}
