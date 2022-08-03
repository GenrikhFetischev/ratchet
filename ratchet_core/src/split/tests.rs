// Copyright 2015-2021 Swim Inc.
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

use crate::framed::{read_next, FramedWrite, Item};
use crate::protocol::{ControlCode, DataCode, HeaderFlags, OpCode};
use crate::split::{FramedIo, Receiver, Sender};
use crate::ws::extension_encode;
use crate::{
    Error, Message, NegotiatedExtension, NoExt, NoExtDecoder, NoExtEncoder, Role, WebSocket,
    WebSocketConfig, WebSocketStream,
};
use bytes::{Bytes, BytesMut};
use ratchet_ext::{ExtensionDecoder, ExtensionEncoder};
use tokio::io::{duplex, DuplexStream};
use tokio::net::TcpStream;

#[test]
fn bounds() {
    fn is<T: Send + Sync + Unpin>() {}

    is::<Sender<TcpStream, NoExt>>();
    is::<Receiver<TcpStream, NoExt>>();
}

impl<S, E> Sender<S, E>
where
    S: WebSocketStream,
    E: ExtensionEncoder,
{
    pub async fn write_frame<A>(&mut self, buf: A, opcode: OpCode, fin: bool) -> Result<(), Error>
    where
        A: AsRef<[u8]>,
    {
        let Sender {
            role, ext_encoder, ..
        } = self;
        let mut split_guard = self.split_writer.lock().await;
        let writer = &mut *split_guard;

        let mut writer_guard = writer.split_writer.lock().await;

        FramedWrite::write(
            &mut writer.writer,
            &mut *writer_guard,
            role.is_server(),
            opcode,
            if fin {
                HeaderFlags::FIN
            } else {
                HeaderFlags::empty()
            },
            buf,
            |payload, header| extension_encode(ext_encoder, payload, header),
        )
        .await
        .map_err(Into::into)
    }
}

impl<S, E> Receiver<S, E>
where
    S: WebSocketStream,
    E: ExtensionDecoder,
{
    pub async fn read_frame(&mut self, read_buffer: &mut BytesMut) -> Result<Item, Error> {
        let Receiver { framed, .. } = self;
        let FramedIo {
            flags,
            max_message_size,
            read_half,
            reader,
            ext_decoder,
            ..
        } = framed;

        read_next(
            read_half,
            reader,
            flags,
            *max_message_size,
            read_buffer,
            ext_decoder,
        )
        .await
    }
}

fn fixture() -> (
    (
        Sender<DuplexStream, NoExtEncoder>,
        Receiver<DuplexStream, NoExtDecoder>,
    ),
    (
        Sender<DuplexStream, NoExtEncoder>,
        Receiver<DuplexStream, NoExtDecoder>,
    ),
) {
    let (server, client) = duplex(512);
    let config = WebSocketConfig::default();

    let server = WebSocket::from_upgraded(
        config,
        server,
        NegotiatedExtension::from(NoExt),
        BytesMut::new(),
        Role::Server,
    )
    .split()
    .unwrap();
    let client = WebSocket::from_upgraded(
        config,
        client,
        NegotiatedExtension::from(NoExt),
        BytesMut::new(),
        Role::Client,
    )
    .split()
    .unwrap();

    (client, server)
}

#[tokio::test]
async fn ping_pong() {
    let ((mut client_tx, mut client_rx), (_server_tx, mut server_rx)) = fixture();
    let payload = "ping!";
    client_tx.write_ping(payload).await.expect("Send failed.");

    let mut read_buf = BytesMut::new();
    let message = server_rx.read(&mut read_buf).await.expect("Read failure");

    assert_eq!(message, Message::Ping(Bytes::from("ping!")));
    assert!(read_buf.is_empty());

    let message = client_rx.read(&mut read_buf).await.expect("Read failure");
    assert_eq!(message, Message::Pong(Bytes::from("ping!")));
    assert!(read_buf.is_empty());
}

#[tokio::test]
async fn reads_unsolicited_pong() {
    let ((_client_tx, mut client_rx), (mut server_tx, _server_rx)) = fixture();
    let payload = "pong!";

    let mut read_buf = BytesMut::new();
    server_tx.write_pong(payload).await.expect("Write failure");

    let message = client_rx.read(&mut read_buf).await.expect("Read failure");
    assert_eq!(message, Message::Pong(Bytes::from(payload)));
    assert!(read_buf.is_empty());
}

#[tokio::test]
async fn empty_control_frame() {
    let ((_client_tx, mut client_rx), (mut server_tx, _server_rx)) = fixture();

    let mut read_buf = BytesMut::new();
    server_tx.write_pong(&[]).await.expect("Write failure");

    let message = client_rx.read(&mut read_buf).await.expect("Read failure");
    assert_eq!(message, Message::Pong(Bytes::new()));
    assert!(read_buf.is_empty());
}

#[tokio::test]
async fn interleaved_control_frames() {
    let ((mut client_tx, _client_rx), (_server_tx, mut server_rx)) = fixture();
    let control_data = "data";

    client_tx
        .write_frame("123", OpCode::DataCode(DataCode::Text), false)
        .await
        .expect("Write failure");
    client_tx
        .write_frame("456", OpCode::DataCode(DataCode::Continuation), false)
        .await
        .expect("Write failure");

    client_tx
        .write_frame(control_data, OpCode::ControlCode(ControlCode::Ping), true)
        .await
        .expect("Write failure");

    client_tx
        .write_frame(control_data, OpCode::ControlCode(ControlCode::Pong), true)
        .await
        .expect("Write failure");

    client_tx
        .write_frame("789", OpCode::DataCode(DataCode::Continuation), true)
        .await
        .expect("Write failure");

    let mut buf = BytesMut::new();
    let message = server_rx.read(&mut buf).await.expect("Read failure");

    assert_eq!(message, Message::Ping(Bytes::from(control_data)));
    assert!(!buf.is_empty());

    let message = server_rx.read(&mut buf).await.expect("Read failure");

    assert_eq!(message, Message::Pong(Bytes::from(control_data)));
    assert!(!buf.is_empty());

    let message = server_rx.read(&mut buf).await.expect("Read failure");

    assert_eq!(message, Message::Text);
    assert!(!buf.is_empty());

    assert_eq!(
        String::from_utf8(buf.to_vec()).expect("Malformatted data received"),
        "123456789"
    );
}

#[tokio::test]
async fn bad_ping_pong_response() {
    let ((mut client_tx, mut client_rx), (mut server_tx, mut server_rx)) = fixture();

    client_tx.write_ping("ping1").await.expect("Write failure");

    let mut buf = BytesMut::new();
    let message = server_rx.read(&mut buf).await.expect("Read failure");

    assert_eq!(message, Message::Ping(Bytes::from("ping1")));
    assert!(buf.is_empty());

    // this needs to be a raw frame read as we don't want to change the contents of the client's
    // control buffer but we still want to make sure that the server responds correctly.
    let item = client_rx.read_frame(&mut buf).await.expect("Read failure");
    assert_eq!(item, Item::Pong(BytesMut::from("ping1")));
    assert!(buf.is_empty());

    server_tx
        .write_frame("bad data", OpCode::ControlCode(ControlCode::Pong), true)
        .await
        .expect("Write failure");

    let error = client_rx.read(&mut buf).await.unwrap_err();
    assert!(error.is_protocol());
}

#[tokio::test]
async fn large_control_frames() {
    {
        let ((mut client_tx, _client_rx), (_server_tx, _server_rx)) = fixture();
        let error = client_tx.write_ping(&[13; 256]).await.unwrap_err();
        assert!(error.is_protocol());
    }
    {
        let ((_client_tx, mut client_rx), (mut server_tx, _server_rx)) = fixture();
        server_tx
            .write_frame(&[13; 256], OpCode::ControlCode(ControlCode::Pong), true)
            .await
            .expect("Write failure");

        let error = client_rx.read(&mut BytesMut::new()).await.unwrap_err();
        assert!(error.is_protocol());
    }
}
