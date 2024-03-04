use std::io;
use std::time::Duration;
use clap::Parser;
use tracing::Level;
use crate::peer::Peer;

mod peer;
mod sender;
mod receiver;
mod error;
mod network;

#[tokio::main]
async fn main() -> io::Result<()> {
    let subscriber = tracing_subscriber::fmt().with_max_level(Level::DEBUG).finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set tracing subscriber");
    let args = Args::parse();
    let period =  args.period;
    let port = args.port;
    let connect = args.connect;
    Peer::new(port, connect).run(Duration::from_secs(period)).await;
    Ok(())
}

/// Command line args
#[derive(Parser, Debug)]
#[command(about, long_about = None)]
struct Args {
    /// send a random gossip message to all the other peers every N seconds
    #[arg(long)]
    period: u64,
    /// Port to start peer on
    #[arg(long)]
    port: u32,
    /// The 'connect_to' arg is None if this peer is first in the network
    #[arg(long)]
    connect: Option<String>,
}
