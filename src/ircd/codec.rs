// irc::proto::IrcCodec doesn't have proper versions?
// give up and wrap it as suggested when the problem happened ages ago:
// https://github.com/tokio-rs/tokio/discussions/3081

use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use irc::proto::{error::ProtocolError, IrcCodec, Message};

pub struct Codec {
    inner: IrcCodec,
}

impl Codec {
    pub fn new(encoding: &str) -> Result<Codec, ProtocolError> {
        IrcCodec::new(encoding).map(|codec| Codec { inner: codec })
    }
}

impl<P> Encoder<P> for Codec {
    type Error = ProtocolError;

    fn encode(&mut self, item: P, dst: &mut BytesMut) -> Result<(), Self::Error> {
        Ok(())
        // self.inner.encode(item, dst)
    }
}

impl Decoder for Codec {
    type Item = Message;
    type Error = ProtocolError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.inner.decode(src)
        //Decoder::decode(&mut self.inner, src)
    }
}
