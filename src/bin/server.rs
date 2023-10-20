use std::{fs::OpenOptions, io, net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc};

use argh::FromArgs;
use chat::message::{ConnectResponse, Message, TextInfo};
use dashmap::DashMap;
use rusqlite::Connection;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
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
                        .unwrap_or_else(|_| "server=INFO".into())
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
                        .unwrap_or_else(|_| "server=INFO".into())
                }))
                .with(tracing_subscriber::fmt::layer().with_writer(io::stdout))
                .init();
        }
    };

    let conn = Connection::open(&args.db_path)?;

    conn.execute(
        "
        CREATE TABLE IF NOT EXISTS channels (
            channel_id INTEGER PRIMARY KEY NOT NULL
        );",
        [],
    )?;
    conn.execute(
        "
        CREATE TABLE IF NOT EXISTS users (
            user_id INTEGER PRIMARY KEY NOT NULL
        );",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS chats (
            chat_id INTEGER PRIMARY KEY NOT NULL,
            channel_id INTEGER REFERENCES channels(channel_id) NOT NULL,
            user_id INTEGER REFERENCES users(user_id) NOT NULL,
            username TEXT NOT NULL,
            message TEXT NOT NULL
        );
    ",
        [],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS notifications (
            notification_id INTEGER PRIMARY KEY NOT NULL,
            user_id REFERENCES users(user_id) NOT NULL,
            channel_id REFERENCES channels(channel_id) NOT NULL
         );
    ",
        [],
    )?;

    drop(conn);

    // initialize map between user_id and task in communication with it
    let user_to_task = Arc::new(DashMap::new());

    let addr = args
        .addr
        .unwrap_or_else(|| SocketAddr::from_str("127.0.0.1:3030").unwrap());
    tracing::info!("Serving on: {addr}");
    let listener = TcpListener::bind(addr).await?;

    loop {
        tokio::select! {
            Ok((stream, _)) = listener.accept() => {

                let db_path = args.db_path.clone();
                let user_to_task = user_to_task.clone();
                tokio::spawn(async move {
                    let e = handle_connection(stream, db_path, user_to_task).await;
                    if let Err(e) = e {
                        tracing::error!("{e}")
                    }
                });
            }
            _ = signal::ctrl_c() => {
                break
            }
            else => {
                break
            }
        }
    }

    tracing::info!("shutting down the server");

    Ok(())
}

async fn handle_connection(
    mut stream: TcpStream,
    db_path: String,
    user_to_task: Arc<DashMap<u64, mpsc::Sender<u64>>>,
) -> color_eyre::Result<()> {
    tracing::info!("received connection");
    let mut data = [0u8; 512];

    let (tx, mut rx) = mpsc::channel(32);

    loop {
        tokio::select! {
            Ok(read) = stream.read(&mut data) => {
                if read == 0 {
                    continue;
                }

                match serde_json::from_slice::<Message>(&data[..read]) {
                    Ok(msg) => {
                        // handle message
                        let res = handle_message(&msg, &db_path, user_to_task.clone(), tx.clone()).await?;
                        if let Some(res) = res {
                            stream
                                .write_all(serde_json::to_string(&res)?.as_bytes())
                                .await?;
                        }
                    }
                    Err(e) => tracing::warn!("error parsing: {e}"),
                };
            }
            Some(chat_id) = rx.recv() => {
                // handle notification
                let conn = Connection::open(&db_path)?;
                // nest because 'stmt' is not Send
                let res = {
                    let mut stmt = conn.prepare("SELECT * FROM chats WHERE chat_id = ?1")?;

                    stmt.query_row([chat_id], |row| {
                        Ok(Message::Text(TextInfo {
                            user_id: row.get(2)?,
                            channel_id: row.get(1)?,
                            username: row.get(3)?,
                            message: row.get(4)?,
                        }))
                    })?
                };

                stream.write_all(serde_json::to_string(&res)?.as_bytes()).await?;
            }
            else => break
        }
    }
    // need to figure out a way to remove k/v from user_to_task, but that requires the user_id

    tracing::debug!("tcp connection closed");

    Ok(())
}

async fn handle_message(
    msg: &Message,
    db_path: &str,
    user_to_task: Arc<DashMap<u64, mpsc::Sender<u64>>>,
    tx: mpsc::Sender<u64>,
) -> color_eyre::Result<Option<Message>> {
    tracing::debug!("received message: {msg:?}");

    let conn = Connection::open(db_path)?;

    let res = match msg {
        Message::Connect(c) => {
            // set up this connection to subscribe to a channel
            tracing::info!("received connect message");

            conn.execute(
                "INSERT INTO notifications (user_id, channel_id) VALUES (?1, ?2);",
                [c.user_id, c.channel_id],
            )?;

            tracing::info!("connecting {}", c.user_id);
            user_to_task.insert(c.user_id, tx);

            Some(Message::ConnectResponse(ConnectResponse::Success))
        }
        Message::Disconnect(d) => {
            use chat::message::DisconnectTy::*;

            tracing::info!("received disconnect message");
            match d.ty {
                All => {
                    tracing::debug!("closing all notifications for {}", d.user_id);
                    conn.execute("DELETE FROM notifications WHERE user_id = ?1;", [d.user_id])?;
                }
                One(v) => {
                    tracing::debug!("closing channel {} notifications for {}", v, d.user_id);
                    conn.execute(
                        "DELETE FROM notifications WHERE user_id = ?1 AND channel_id = ?2",
                        [d.user_id, v],
                    )?;
                }
            }

            Some(Message::Success)
        }
        Message::Text(t) => {
            // send this message to each subscribed party
            tracing::info!("received text message");

            let chat_id = {
                let mut stmt = conn.prepare("INSERT INTO chats (channel_id, user_id, username, message) VALUES (?1, ?2, ?3, ?4) RETURNING chat_id")?;
                stmt.query_row(
                    (t.channel_id, t.user_id, &t.username, t.message.trim()),
                    |row| row.get(0),
                )?
            };
            let users: Vec<u64> = {
                let mut stmt = conn
                    .prepare("SELECT DISTINCT user_id FROM notifications WHERE channel_id = ?1")?;
                let v = stmt
                    .query_map([t.channel_id], |row| row.get::<_, u64>(0))?
                    .map(Result::unwrap)
                    .collect();
                // if we don't assign the result to a variable and return directly, stmt doesn't live long enough
                #[allow(clippy::let_and_return)]
                v
            };
            for user_id in users {
                tracing::info!("notifying {user_id} about {chat_id}");
                if let Some(tx) = user_to_task.get(&user_id) {
                    tx.send(chat_id).await?;
                }
            }

            Some(Message::Success)
        }
        Message::ConnectResponse(_c) => {
            tracing::error!("received connect response on server. this is unintended behavior");

            Some(Message::InvalidMessage)
        }
        Message::InvalidMessage => {
            tracing::warn!("received InvalidMessage");

            None
        }
        Message::Success => {
            tracing::info!("received Success");

            None
        }
    };

    Ok(res)
}

/// Chat Server
///
/// Allow for people to chat
#[derive(Debug, FromArgs)]
struct Args {
    /// path to the database
    #[argh(option, default = "String::from(\"chat.db\")")]
    pub db_path: String,
    /// level to log to
    #[argh(option)]
    pub log_level: Option<String>,
    /// where to log to
    #[argh(option)]
    pub log_file: Option<PathBuf>,
    /// where to serve on
    #[argh(option)]
    pub addr: Option<SocketAddr>,
}
