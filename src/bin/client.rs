use std::{
    fs::OpenOptions,
    io::{self, Write},
    net::SocketAddr,
    path::PathBuf,
    str::FromStr,
    thread,
};

use argh::FromArgs;
use chat::message::{ConnectInfo, Disconnect, DisconnectTy, Message, TextInfo};
use futures::executor::block_on;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    signal,
    sync::mpsc,
};
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let args: Args = argh::from_env();
    match args.log_file {
        Some(path) => {
            tracing_subscriber::registry()
                .with(args.log_level.map(Into::into).unwrap_or_else(|| {
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "client=WARN".into())
                }))
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(OpenOptions::new().create(true).append(true).open(path)?),
                )
                .init();
        }
        None => {
            tracing_subscriber::registry()
                .with(args.log_level.map(Into::into).unwrap_or_else(|| {
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "client=WARN".into())
                }))
                .with(tracing_subscriber::fmt::layer().with_writer(io::stdout))
                .init();
        }
    };

    let addr = args
        .addr
        .unwrap_or_else(|| SocketAddr::from_str("127.0.0.1:3030").unwrap());
    tracing::info!("connecting to: {addr}");

    let mut stream = TcpStream::connect(addr).await?;

    let connect = Message::Connect(ConnectInfo {
        user_id: args.user_id,
        channel_id: args.channel_id,
    });

    tracing::info!("sending connect");
    stream
        .write_all(serde_json::to_string(&connect)?.as_bytes())
        .await?;

    let res = read_message(&mut stream).await?;
    tracing::info!("received response: {res:?}");

    // read input
    let (tx, mut rx) = mpsc::channel(4);
    // using a thread and futures, because as a tokio task, this blocks the executor and prevents the program from exiting.
    // if I turn this into a spawn_blocking in tokio, the program stops reading input, so instead I've made an OS thread
    // which won't prevent the program from exiting and will automatically be cleaned up.
    thread::spawn(move || {
        let mut buf = String::new();
        loop {
            buf.clear();
            io::stdin().read_line(&mut buf).unwrap();
            block_on(tx.send(buf.clone())).unwrap();
        }
    });

    loop {
        print!("::> ");
        io::stdout().flush().unwrap();
        tokio::select! {
            msg = read_message(&mut stream) => {
                let Ok(msg) = msg else {
                    tracing::warn!("server disconnected: {}", msg.unwrap_err());
                    return Ok(());
                };
                tracing::info!("new message received: {msg:?}");
                if let Message::Text(t) = msg {
                    // clear line and put cursor at start
                    print!("\r{}\r", " ".repeat(100));
                    println!("{}\t-> {}", t.username, t.message);
                }
            }
            Some(buf) = rx.recv() => {
                let msg = Message::Text(TextInfo { user_id: args.user_id, channel_id: args.channel_id, username: args.username.clone(), message: buf });
                tracing::info!("sending message: {msg:?}");
                stream.write_all(serde_json::to_string(&msg)?.as_bytes()).await?;
            }
            Ok(()) = signal::ctrl_c() => {
                break
            }
            else => {
                break
            }
        }
    }

    // exit the chats
    tracing::info!("disconnecting");
    stream
        .write_all(
            serde_json::to_string(&Message::Disconnect(Disconnect {
                user_id: args.user_id,
                ty: DisconnectTy::All,
            }))?
            .as_bytes(),
        )
        .await?;

    Ok(())
}

async fn read_message(stream: &mut TcpStream) -> color_eyre::Result<Message> {
    let mut data = [0u8; 512];
    loop {
        let read = stream.read(&mut data).await?;
        if read == 0 {
            continue;
        }
        break serde_json::from_slice::<Message>(&data[..read]).map_err(Into::into);
    }
}

/// Connect to chat server
#[derive(Debug, FromArgs)]
struct Args {
    /// channel id to connect to
    #[argh(option, short = 'c')]
    pub channel_id: u64,
    /// user id to connect to
    #[argh(option)]
    pub user_id: u64,
    /// username
    #[argh(option, short = 'u')]
    pub username: String,
    /// level to log to
    #[argh(option)]
    pub log_level: Option<String>,
    /// where to log to
    #[argh(option)]
    pub log_file: Option<PathBuf>,
    /// where to connect to
    #[argh(option)]
    pub addr: Option<SocketAddr>,
}
