use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::io;
use std::io::Error;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use actix::dev::MessageResponse;
use actix::io::{FramedWrite, WriteHandler};
use actix_rt::spawn;
use actix::prelude::*;
use tokio::io::{AsyncWriteExt, split, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::FramedRead;
//use tokio_io::_tokio_codec::FramedRead;
use tracing::{debug, info, warn};
use tracing::field::debug;
use tracing::log::error;
use crate::codec::{InCodec, OutCodec};
use crate::connection::{InConnection, OutConnection};
use crate::message::{InMessage, OutMessage};
use crate::message::Request::{MessageRequest, TryHandshake};


pub struct Peer {
    /// address being listened by peer
    socket_addr: SocketAddr,
    /// messaging period in which messages to peers are sent
    period: Duration,
    /// initial peer to connect and get [`SocketAddr`] of all the peers in network
    connect_to: Option<SocketAddr>,
    /// peers to connect and to respond other peers
    peers: HashSet<SocketAddr>,
    /// established connections (actors to send messages)
    connections: HashSet<Connection>,
}

#[derive(Eq, Hash, PartialEq)]
pub enum Connection {
    In(Addr<InConnection>),
    Out(Addr<OutConnection>),
}

/// Peer running on the current process or host
impl Peer {
    pub fn new(port: u32, period: Duration, connect_to: Option<SocketAddr>) -> Self {
        let socket_addr = format!("127.0.0.1:{}", port);
        let socket_addr = SocketAddr::from_str(&socket_addr).unwrap();

        Self {
            socket_addr,
            period,
            connect_to,
            peers: Default::default(),
            //from_connections: Default::default()
            connections: Default::default(),
        }
    }

    // set up peer to start listen incoming connections
    fn start_listening(&self, peer_addr: Addr<Peer>) {
        let addr = self.socket_addr.clone();
        let period = self.period;
        spawn(async move {
            let listener = TcpListener::bind(addr).await.unwrap();
            while let Ok((stream, addr)) = listener.accept().await {
                debug!("peer [{addr}] connected");
                let peer = peer_addr.clone();
                InConnection::create(|ctx| {
                    let (r, w) = split(stream);
                    // reading from remote peer connection
                    InConnection::add_stream(FramedRead::new(r, InCodec), ctx);
                    // writing to remote peer connection
                    InConnection::new(addr, peer, FramedWrite::new(w, InCodec, ctx), period)
                });
            }
        });
    }

    fn contains_conn(&self, addr: &SocketAddr) -> bool {
        self.peers.iter()
            .any(|p| p.eq(addr))
    }
}

impl Actor for Peer {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        // Peer is the first in the network
        if self.connect_to.is_none() {
            info!("Peer has started on [{}]. Waiting for incoming connections", self.socket_addr);
            self.start_listening(ctx.address());
            return;
        }
        // Trying to connect initial peer
        self.start_listening(ctx.address());
        let period = self.period;
        let connect_to = self.connect_to.unwrap();
        let peer_ctx_address = ctx.address().clone();
        info!("Peer has started on [{}]. Trying to connect [{connect_to}]", self.socket_addr);

        ctx.spawn(async move {
            TcpStream::connect(connect_to).await
        }
            .into_actor(self)
            .map_err(move |e, _act, ctx| {
                error!("Couldn't establish connection with peer: {connect_to}, error {e}");
                ctx.stop();
            })
            .map(move |res, actor, _ctx| {
                let stream = res.unwrap();
                let remote_peer_addr = stream.peer_addr().unwrap();
                let (r, w) = split(stream);
                let initial_peer = OutConnection::create(|ctx| {
                    OutConnection::add_stream(FramedRead::new(r, OutCodec), ctx);
                    OutConnection::new(
                        actor.socket_addr,
                        remote_peer_addr,
                        peer_ctx_address,
                        FramedWrite::new(w, OutCodec, ctx),
                        period)
                });
                // try perform handshake with remote peer
                let _ = initial_peer.try_send(
                    OutMessage::Request(
                        TryHandshake{
                            token: b"secret".to_vec(),
                            sender: actor.socket_addr,
                            receiver: connect_to})
                );
            })
        );
    }
}

#[derive(Debug, Message)]
#[rtype(result = "()")]
pub(crate) struct AddInConnection(pub Addr<InConnection>);

impl Handler<AddInConnection> for Peer {
    type Result = ();

    fn handle(&mut self, msg: AddInConnection, _ctx: &mut Self::Context) -> Self::Result {
        let _ = self.connections.insert(Connection::In(msg.0));
        debug!("in connection has been added to connections");
        //let _ = self.peers.insert(msg.1);
    }
}

#[derive(Debug, Message)]
#[rtype(result = "()")]
pub(crate) struct AddOutConnection(pub Addr<OutConnection>);

impl Handler<AddOutConnection> for Peer {
    type Result = ();

    fn handle(&mut self, msg: AddOutConnection, _ctx: &mut Self::Context) -> Self::Result {
        let _ = self.connections.insert(Connection::Out(msg.0));
        debug!("out connection has been added to connections");
    }
}

#[derive(Debug, Message)]
#[rtype(result = "()")]
pub struct AddPeer(pub SocketAddr);

impl Handler<AddPeer> for Peer {
    type Result = ();

    fn handle(&mut self, msg: AddPeer, ctx: &mut Self::Context) -> Self::Result {
        debug!("peer [{}] added to peers list", msg.0);
        self.peers.insert(msg.0);
    }
}

#[derive(Debug, Message)]
#[rtype(result = "()")]
pub struct AddPeers(pub HashSet<SocketAddr>);

impl Handler<AddPeers> for Peer {
    type Result = ();

    fn handle(&mut self, msg: AddPeers, ctx: &mut Self::Context) -> Self::Result {
        let mut peers_to_add = msg.0;
        // we don't need to store our own peer address here
        peers_to_add.remove(&self.socket_addr);
        self.peers.extend(peers_to_add);
        let peers_to_connect = self.peers.clone();
        debug!("peers hash set: {:?}", &self.peers);
        // establish connection with required peers
        ctx.notify(ConnectPeers(peers_to_connect));
    }
}

#[derive(Debug, Message)]
#[rtype(result = "()")]
pub struct AddConnectedPeer(pub SocketAddr);

impl Handler<AddConnectedPeer> for Peer {
    type Result = ();

    fn handle(&mut self, msg: AddConnectedPeer, _ctx: &mut Self::Context) -> Self::Result {
        if self.peers.insert(msg.0) {
            debug!("peer [{}] has been added to peers", msg.0)
        }
    }
}

/// notify ourself than we need to establish connection with peers
#[derive(Debug, Message)]
#[rtype(result = "()")]
struct ConnectPeers(HashSet<SocketAddr>);

impl Handler<ConnectPeers> for Peer {
    type Result = ();

    fn handle(&mut self, msg: ConnectPeers, peer_ctx: &mut Self::Context) -> Self::Result {
        let peer_actor = peer_ctx.address();
        // We don't need to add already connected peers
        let peers_to_connect = msg.0.iter()
            .filter_map(|to_connect| {
                return if self.peers.iter().any(|connected| connected.eq(to_connect)) {
                    Some(*to_connect)
                } else { None }
            })
            .collect::<HashSet<SocketAddr>>();

        let peers_to_connect = Arc::new(peers_to_connect);
        let peer_addr = self.socket_addr.clone();
        let period = self.period;

        peer_ctx.spawn(async move {
            // TODO define retry count
            let mut errors = HashSet::new();
            for peer in peers_to_connect.iter() {
                let stream = TcpStream::connect(peer).await;
                if let Ok(stream) = stream {
                    OutConnection::create(|ctx| {
                        let (r, w) = split(stream);
                        OutConnection::add_stream(FramedRead::new(r, OutCodec), ctx);
                        OutConnection::new(
                            peer_addr,
                            *peer,
                            peer_actor.clone(),
                            FramedWrite::new(w, OutCodec, ctx),
                            period)
                    });
                } else {
                    warn!("couldn't establish connection with peer: {peer}");
                    errors.insert(*peer);
                }
            }
            errors
        }
            .into_actor(self)
            .map(|errors, _actor, ctx| {
                // Retry establishing connection later TODO wait a few seconds before this action
                if !errors.is_empty() {
                    debug!("retrying to establish connection with peers: {:?} after 3 seconds", errors);
                    ctx.notify_later(ConnectPeers(errors), Duration::from_secs(3));
                }
            }));
    }
}

#[derive(Debug, Message)]
#[rtype(result = "HashSet<SocketAddr>")]
pub(crate) struct GetPeers;

impl Handler<GetPeers> for Peer {
    type Result = MessageResult<GetPeers>;

    fn handle(&mut self, _msg: GetPeers, _ctx: &mut Self::Context) -> Self::Result {
        let mut peers = self.peers.clone();
        //peers.insert(self.socket_addr);
        MessageResult(peers)
    }
}

/// send random messages to connected peers
#[derive(Debug, Message)]
#[rtype(result = "()")]
pub struct SendMessages(pub String);

impl Handler<SendMessages> for Peer {
    type Result = ();

    fn handle(&mut self, msgs: SendMessages, ctx: &mut Self::Context) -> Self::Result {
        // start sending messages with specified [`period`]
        warn!("peer has started sending messages : [{}]", &msgs.0);
        warn!("peers size: {}", self.peers.len());
        warn!("connections size: {}", self.connections.len());
        ctx.run_interval(self.period, move |actor, _ctx| {
            for conn in actor.connections.iter() {
                match conn {
                    Connection::In(c) => {
                        let _ = c.try_send(InMessage::Request(MessageRequest(msgs.0.clone(), actor.socket_addr)));
                    }
                    Connection::Out(c) => {
                        let _ = c.try_send(OutMessage::Request(MessageRequest(msgs.0.clone(), actor.socket_addr)));
                    }
                }
            }
        });
    }
}

fn gen_rnd_msg() -> String {
    use random_word::Lang;
    let msg = random_word::gen(Lang::En);
    String::from(msg)
}