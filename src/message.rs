//! Types of messages
//!

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub enum Message {
    Text(TextInfo),
    Connect(ConnectInfo),
    Disconnect(Disconnect),
    ConnectResponse(ConnectResponse),
    Success,
    InvalidMessage,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct TextInfo {
    pub user_id: u64,
    pub channel_id: u64,
    pub username: String,
    pub message: String,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy)]
pub struct ConnectInfo {
    pub user_id: u64,
    pub channel_id: u64,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy)]
pub enum ConnectResponse {
    #[default]
    Success,
    NoChannel,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy)]
pub struct Disconnect {
    pub user_id: u64,
    pub ty: DisconnectTy,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy)]
pub enum DisconnectTy {
    #[default]
    All,
    One(u64),
}
