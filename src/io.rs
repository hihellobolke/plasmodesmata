use std::rc::Rc;
use std::io::{self, Read, Write};
use std::net::Shutdown;

use futures::prelude::*;
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::io::shutdown;
use bytes::{BytesMut, Bytes, IntoBuf};
use h2::{self, server as h2s, client as h2c};
use tokio_core::net::TcpStream;

#[async]
pub fn copy_from_h2<
    W: AsyncWrite + 'static,
    B: Stream<Item = Bytes, Error = h2::Error> + 'static,
>(
    src: B,
    mut dst: W,
) -> Result<usize, h2::Error> {
    let mut counter = 0;
    #[async]
    for buf in src {
        let mut buf = buf.into_buf();
        let n = poll!(dst.write_buf(&mut buf))?;
        counter += n;
    }
    await!(shutdown(dst))?;
    println!("tcp remote close");
    Ok(counter)
}

pub trait SendData<B: IntoBuf> {
    fn send_data(&mut self, data: B, end_of_stream: bool) -> Result<(), h2::Error>;
}

impl<B: IntoBuf> SendData<B> for h2c::Stream<B> {
    fn send_data(&mut self, data: B, end_of_stream: bool) -> Result<(), h2::Error> {
        h2c::Stream::send_data(self, data, end_of_stream)
    }
}
impl<B: IntoBuf> SendData<B> for h2s::Stream<B> {
    fn send_data(&mut self, data: B, end_of_stream: bool) -> Result<(), h2::Error> {
        h2s::Stream::send_data(self, data, end_of_stream)
    }
}

#[async]
pub fn copy_to_h2<R: AsyncRead + 'static, H: SendData<Bytes> + 'static>(
    mut src: R,
    mut dst: H,
) -> Result<usize, h2::Error> {
    let mut counter = 0;
    let mut buf = BytesMut::with_capacity(1024);
    loop {
        let n = poll!(src.read_buf(&mut buf))?;
        if n == 0 {
            dst.send_data(buf.take().freeze(), true)?;
            break;
        } else {
            dst.send_data(buf.take().freeze(), false)?;
        }
        counter += n;
    }
    Ok(counter)
}

// This is a custom type used to have a custom implementation of the
// `AsyncWrite::shutdown` method which actually calls `TcpStream::shutdown` to
// notify the remote end that we're done writing.
#[derive(Clone)]
pub struct Socket(Rc<TcpStream>);

impl Socket {
    pub fn new(s: TcpStream) -> Socket {
        Socket(Rc::new(s))
    }
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsyncRead for Socket {}

impl AsyncWrite for Socket {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        try!(self.0.shutdown(Shutdown::Write));
        Ok(().into())
    }
}
