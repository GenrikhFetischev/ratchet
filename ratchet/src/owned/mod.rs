// Copyright 2015-2021 SWIM.AI inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Sink;
use tokio_util::codec::Framed;

pub use builder::{WebSocketClientBuilder, WebSocketServerBuilder};

use crate::codec::{Codec, FragmentBuffer};
use crate::errors::Error;
use crate::extensions::NegotiatedExtension;
use crate::handshake::{exec_client_handshake, HandshakeResult, ProtocolRegistry};
use crate::protocol::frame::Message;
use crate::{
    Deflate, Extension, ExtensionHandshake, Request, Role, WebSocketConfig, WebSocketStream,
};

mod builder;

pub struct WebSocket<S, E = Deflate> {
    inner: WebSocketInner<S, E>,
}

pub struct WebSocketInner<S, E> {
    framed: Framed<S, Codec<FragmentBuffer>>,
    role: Role,
    extension: NegotiatedExtension<E>,
    config: WebSocketConfig,
    _priv: (),
}

impl<S, E> WebSocket<S, E>
where
    S: WebSocketStream,
    E: Extension,
{
    pub fn role(&self) -> Role {
        self.inner.role
    }

    pub async fn config(&self) -> &WebSocketConfig {
        &self.inner.config
    }
}

pub async fn client<S, E>(
    config: WebSocketConfig,
    mut stream: S,
    request: Request,
    codec: Codec<FragmentBuffer>,
    extension: E,
    subprotocols: ProtocolRegistry,
) -> Result<(WebSocket<S, E::Extension>, Option<String>), Error>
where
    S: WebSocketStream,
    E: ExtensionHandshake,
{
    let HandshakeResult {
        protocol,
        extension,
    } = exec_client_handshake(&mut stream, request, extension, subprotocols).await?;
    let socket = WebSocket {
        inner: WebSocketInner {
            framed: Framed::new(stream, codec),
            role: Role::Client,
            extension,
            config,
            _priv: (),
        },
    };
    Ok((socket, protocol))
}

impl<S, E> Sink<Message> for WebSocket<S, E>
where
    S: WebSocketStream,
    E: Extension + Unpin,
{
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner.framed).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        Pin::new(&mut self.inner.framed).start_send(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner.framed)
            .poll_flush(cx)
            .map_err(Into::into)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner.framed)
            .poll_close(cx)
            .map_err(Into::into)
    }
}
